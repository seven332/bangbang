//! vmnet lifecycle boundary types for future macOS host networking.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::fmt;
use std::marker::PhantomData;
use std::ops::Range;
use std::ptr::{self, NonNull};
use std::str::FromStr;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use bangbang_runtime::network::{
    GuestMacAddress, VIRTIO_NET_MAX_BUFFER_SIZE, VIRTIO_NET_MAX_MTU, VIRTIO_NET_MIN_MTU,
    VIRTIO_NET_QUEUE_SIZE,
};
use block2::{Block, RcBlock};
use dispatch2::{DispatchQueue, DispatchQueueAttr, DispatchRetained};

pub const VMNET_HOST_MODE_VALUE: u32 = 1000;
pub const VMNET_SHARED_MODE_VALUE: u32 = 1001;
pub const VMNET_BRIDGED_MODE_VALUE: u32 = 1002;

pub const VMNET_SUCCESS_VALUE: u32 = 1000;
pub const VMNET_FAILURE_VALUE: u32 = 1001;
pub const VMNET_MEM_FAILURE_VALUE: u32 = 1002;
pub const VMNET_INVALID_ARGUMENT_VALUE: u32 = 1003;
pub const VMNET_SETUP_INCOMPLETE_VALUE: u32 = 1004;
pub const VMNET_INVALID_ACCESS_VALUE: u32 = 1005;
pub const VMNET_PACKET_TOO_BIG_VALUE: u32 = 1006;
pub const VMNET_BUFFER_EXHAUSTED_VALUE: u32 = 1007;
pub const VMNET_TOO_MANY_PACKETS_VALUE: u32 = 1008;
pub const VMNET_SHARING_SERVICE_BUSY_VALUE: u32 = 1009;
pub const VMNET_NOT_AUTHORIZED_VALUE: u32 = 1010;

pub const VMNET_HOST_DEVICE_NAME_HOST: &str = "vmnet:host";
pub const VMNET_HOST_DEVICE_NAME_SHARED: &str = "vmnet:shared";
pub const VMNET_HOST_DEVICE_NAME_BRIDGED_PREFIX: &str = "vmnet:bridged:";

pub const DEFAULT_VMNET_COMPLETION_TIMEOUT: Duration = Duration::from_secs(5);
pub(crate) const VMNET_MAX_PACKETS_PER_OPERATION: usize = 200;
pub(crate) const VMNET_MAX_BYTES_PER_OPERATION: usize = 256 * 1024;
pub const VMNET_INTERFACE_PACKETS_AVAILABLE_VALUE: u32 = 1 << 0;
const VMNET_MAC_ADDRESS_STRING_LEN: usize = 17;
const VMNET_INTERFACE_ID_LEN: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetMode {
    Host,
    Shared,
    Bridged,
}

impl VmnetMode {
    pub const fn raw_value(self) -> u32 {
        match self {
            Self::Host => VMNET_HOST_MODE_VALUE,
            Self::Shared => VMNET_SHARED_MODE_VALUE,
            Self::Bridged => VMNET_BRIDGED_MODE_VALUE,
        }
    }
}

impl fmt::Display for VmnetMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Host => "host",
            Self::Shared => "shared",
            Self::Bridged => "bridged",
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VmnetStatus {
    Success,
    Failure,
    MemoryFailure,
    InvalidArgument,
    SetupIncomplete,
    InvalidAccess,
    PacketTooBig,
    BufferExhausted,
    TooManyPackets,
    SharingServiceBusy,
    NotAuthorized,
    Unknown(u32),
}

impl VmnetStatus {
    pub const fn from_raw(value: u32) -> Self {
        match value {
            VMNET_SUCCESS_VALUE => Self::Success,
            VMNET_FAILURE_VALUE => Self::Failure,
            VMNET_MEM_FAILURE_VALUE => Self::MemoryFailure,
            VMNET_INVALID_ARGUMENT_VALUE => Self::InvalidArgument,
            VMNET_SETUP_INCOMPLETE_VALUE => Self::SetupIncomplete,
            VMNET_INVALID_ACCESS_VALUE => Self::InvalidAccess,
            VMNET_PACKET_TOO_BIG_VALUE => Self::PacketTooBig,
            VMNET_BUFFER_EXHAUSTED_VALUE => Self::BufferExhausted,
            VMNET_TOO_MANY_PACKETS_VALUE => Self::TooManyPackets,
            VMNET_SHARING_SERVICE_BUSY_VALUE => Self::SharingServiceBusy,
            VMNET_NOT_AUTHORIZED_VALUE => Self::NotAuthorized,
            value => Self::Unknown(value),
        }
    }

    pub const fn raw_value(self) -> u32 {
        match self {
            Self::Success => VMNET_SUCCESS_VALUE,
            Self::Failure => VMNET_FAILURE_VALUE,
            Self::MemoryFailure => VMNET_MEM_FAILURE_VALUE,
            Self::InvalidArgument => VMNET_INVALID_ARGUMENT_VALUE,
            Self::SetupIncomplete => VMNET_SETUP_INCOMPLETE_VALUE,
            Self::InvalidAccess => VMNET_INVALID_ACCESS_VALUE,
            Self::PacketTooBig => VMNET_PACKET_TOO_BIG_VALUE,
            Self::BufferExhausted => VMNET_BUFFER_EXHAUSTED_VALUE,
            Self::TooManyPackets => VMNET_TOO_MANY_PACKETS_VALUE,
            Self::SharingServiceBusy => VMNET_SHARING_SERVICE_BUSY_VALUE,
            Self::NotAuthorized => VMNET_NOT_AUTHORIZED_VALUE,
            Self::Unknown(value) => value,
        }
    }

    const fn name(self) -> Option<&'static str> {
        match self {
            Self::Success => Some("VMNET_SUCCESS"),
            Self::Failure => Some("VMNET_FAILURE"),
            Self::MemoryFailure => Some("VMNET_MEM_FAILURE"),
            Self::InvalidArgument => Some("VMNET_INVALID_ARGUMENT"),
            Self::SetupIncomplete => Some("VMNET_SETUP_INCOMPLETE"),
            Self::InvalidAccess => Some("VMNET_INVALID_ACCESS"),
            Self::PacketTooBig => Some("VMNET_PACKET_TOO_BIG"),
            Self::BufferExhausted => Some("VMNET_BUFFER_EXHAUSTED"),
            Self::TooManyPackets => Some("VMNET_TOO_MANY_PACKETS"),
            Self::SharingServiceBusy => Some("VMNET_SHARING_SERVICE_BUSY"),
            Self::NotAuthorized => Some("VMNET_NOT_AUTHORIZED"),
            Self::Unknown(_) => None,
        }
    }
}

impl fmt::Display for VmnetStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.name() {
            Some(name) => f.write_str(name),
            None => f.write_str("unknown vmnet status"),
        }
    }
}

impl fmt::Debug for VmnetStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetOperation {
    StartInterface,
    EnablePacketEvents,
    DisablePacketEvents,
    DrainPacketEvents,
    StopInterface,
    ReadPackets,
    WritePackets,
}

impl fmt::Display for VmnetOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::StartInterface => "vmnet_start_interface",
            Self::EnablePacketEvents => "vmnet_interface_set_event_callback(enable)",
            Self::DisablePacketEvents => "vmnet_interface_set_event_callback(disable)",
            Self::DrainPacketEvents => "vmnet packet-event callback drain",
            Self::StopInterface => "vmnet_stop_interface",
            Self::ReadPackets => "vmnet_read",
            Self::WritePackets => "vmnet_write",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetCompletionError {
    TimedOut,
    ChannelClosed,
}

impl fmt::Display for VmnetCompletionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::TimedOut => "completion timed out",
            Self::ChannelClosed => "completion channel closed",
        })
    }
}

impl std::error::Error for VmnetCompletionError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmnetError {
    operation: VmnetOperation,
    status: VmnetStatus,
    completion: Option<VmnetCompletionError>,
}

impl VmnetError {
    pub const fn new(operation: VmnetOperation, status: VmnetStatus) -> Self {
        Self {
            operation,
            status,
            completion: None,
        }
    }

    const fn completion(operation: VmnetOperation, completion: VmnetCompletionError) -> Self {
        Self {
            operation,
            status: VmnetStatus::Failure,
            completion: Some(completion),
        }
    }

    pub const fn operation(&self) -> VmnetOperation {
        self.operation
    }

    pub const fn status(&self) -> VmnetStatus {
        self.status
    }

    pub const fn completion_error(&self) -> Option<VmnetCompletionError> {
        self.completion
    }
}

impl fmt::Display for VmnetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.completion {
            Some(completion) => write!(f, "{} {completion}", self.operation),
            None => write!(f, "{} failed with {}", self.operation, self.status),
        }
    }
}

impl std::error::Error for VmnetError {}

#[derive(Clone, PartialEq, Eq)]
pub struct VmnetInterfaceConfig {
    mode: VmnetMode,
    bridged_interface_name: Option<String>,
    guest_mac: Option<GuestMacAddress>,
    mtu: Option<u16>,
}

impl VmnetInterfaceConfig {
    pub const fn host() -> Self {
        Self {
            mode: VmnetMode::Host,
            bridged_interface_name: None,
            guest_mac: None,
            mtu: None,
        }
    }

    pub const fn shared() -> Self {
        Self {
            mode: VmnetMode::Shared,
            bridged_interface_name: None,
            guest_mac: None,
            mtu: None,
        }
    }

    pub fn from_host_dev_name(host_dev_name: &str) -> Result<Self, VmnetHostDeviceNameConfigError> {
        match host_dev_name {
            VMNET_HOST_DEVICE_NAME_HOST => Ok(Self::host()),
            VMNET_HOST_DEVICE_NAME_SHARED => Ok(Self::shared()),
            name => match name.strip_prefix(VMNET_HOST_DEVICE_NAME_BRIDGED_PREFIX) {
                Some(interface_name) => Self::bridged(interface_name)
                    .map_err(|source| VmnetHostDeviceNameConfigError::BridgedInterface { source }),
                None => Err(VmnetHostDeviceNameConfigError::UnsupportedHostDeviceName),
            },
        }
    }

    pub fn bridged(interface_name: impl Into<String>) -> Result<Self, VmnetInterfaceConfigError> {
        let interface_name = interface_name.into();
        if interface_name.is_empty() {
            return Err(VmnetInterfaceConfigError::EmptyBridgedInterfaceName);
        }
        if interface_name.as_bytes().contains(&0) {
            return Err(VmnetInterfaceConfigError::InteriorNulInBridgedInterfaceName);
        }
        if interface_name.bytes().any(|byte| byte.is_ascii_control()) {
            return Err(VmnetInterfaceConfigError::ControlCharacterInBridgedInterfaceName);
        }

        Ok(Self {
            mode: VmnetMode::Bridged,
            bridged_interface_name: Some(interface_name),
            guest_mac: None,
            mtu: None,
        })
    }

    pub const fn mode(&self) -> VmnetMode {
        self.mode
    }

    pub fn bridged_interface_name(&self) -> Option<&str> {
        self.bridged_interface_name.as_deref()
    }

    pub const fn guest_mac(&self) -> Option<GuestMacAddress> {
        self.guest_mac
    }

    pub const fn mtu(&self) -> Option<u16> {
        self.mtu
    }

    #[must_use]
    pub const fn with_guest_mac(mut self, guest_mac: Option<GuestMacAddress>) -> Self {
        self.guest_mac = guest_mac;
        self
    }

    #[must_use]
    pub const fn with_mtu(mut self, mtu: Option<u16>) -> Self {
        self.mtu = mtu;
        self
    }
}

impl fmt::Debug for VmnetInterfaceConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetInterfaceConfig")
            .field("mode", &self.mode)
            .field(
                "bridged_interface_name",
                &self.bridged_interface_name.as_ref().map(|_| "<configured>"),
            )
            .field("guest_mac", &self.guest_mac.map(|_| "<configured>"))
            .field("mtu", &self.mtu.map(|_| "<configured>"))
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmnetHostDeviceNameConfigError {
    UnsupportedHostDeviceName,
    BridgedInterface { source: VmnetInterfaceConfigError },
}

impl fmt::Display for VmnetHostDeviceNameConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedHostDeviceName => f.write_str(
                "unsupported vmnet host_dev_name; expected vmnet:host, vmnet:shared, or vmnet:bridged:<interface>",
            ),
            Self::BridgedInterface { source } => {
                write!(f, "invalid vmnet bridged host_dev_name: {source}")
            }
        }
    }
}

impl std::error::Error for VmnetHostDeviceNameConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::UnsupportedHostDeviceName => None,
            Self::BridgedInterface { source } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetInterfaceConfigError {
    EmptyBridgedInterfaceName,
    InteriorNulInBridgedInterfaceName,
    ControlCharacterInBridgedInterfaceName,
}

impl fmt::Display for VmnetInterfaceConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyBridgedInterfaceName => {
                f.write_str("vmnet bridged interface name must not be empty")
            }
            Self::InteriorNulInBridgedInterfaceName => {
                f.write_str("vmnet bridged interface name must not contain NUL bytes")
            }
            Self::ControlCharacterInBridgedInterfaceName => f.write_str(
                "vmnet bridged interface name must not contain ASCII control characters",
            ),
        }
    }
}

impl std::error::Error for VmnetInterfaceConfigError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetInterfaceDescriptorError {
    CreateDictionaryFailed,
    InteriorNulInBridgedInterfaceName,
    InteriorNulInMacAddress,
    MissingVmnetKey(&'static str),
}

impl fmt::Display for VmnetInterfaceDescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateDictionaryFailed => {
                f.write_str("failed to create vmnet interface descriptor")
            }
            Self::InteriorNulInBridgedInterfaceName => {
                f.write_str("vmnet bridged interface name must not contain NUL bytes")
            }
            Self::InteriorNulInMacAddress => {
                f.write_str("vmnet MAC address must not contain NUL bytes")
            }
            Self::MissingVmnetKey(key) => write!(f, "vmnet key symbol {key} is null"),
        }
    }
}

impl std::error::Error for VmnetInterfaceDescriptorError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetInterfaceParameterField {
    ResultDictionary,
    MacAddress,
    EffectiveMtu,
    MaximumPacketSize,
    InterfaceId,
    ReadMaximumPackets,
    WriteMaximumPackets,
    DirectVirtioHeader,
}

impl fmt::Display for VmnetInterfaceParameterField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ResultDictionary => "result dictionary",
            Self::MacAddress => "MAC address",
            Self::EffectiveMtu => "effective MTU",
            Self::MaximumPacketSize => "maximum packet size",
            Self::InterfaceId => "interface identifier",
            Self::ReadMaximumPackets => "read batch maximum",
            Self::WriteMaximumPackets => "write batch maximum",
            Self::DirectVirtioHeader => "direct virtio-header mode",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetInterfaceParameterProblem {
    Missing,
    WrongType,
    Malformed,
    OutOfRange,
    ConflictsWithRequest,
}

impl fmt::Display for VmnetInterfaceParameterProblem {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Missing => "is missing",
            Self::WrongType => "has the wrong XPC type",
            Self::Malformed => "is malformed",
            Self::OutOfRange => "is outside supported bounds",
            Self::ConflictsWithRequest => "conflicts with requested configuration",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmnetInterfaceParameterError {
    field: VmnetInterfaceParameterField,
    problem: VmnetInterfaceParameterProblem,
}

impl VmnetInterfaceParameterError {
    const fn new(
        field: VmnetInterfaceParameterField,
        problem: VmnetInterfaceParameterProblem,
    ) -> Self {
        Self { field, problem }
    }

    pub const fn field(&self) -> VmnetInterfaceParameterField {
        self.field
    }

    pub const fn problem(&self) -> VmnetInterfaceParameterProblem {
        self.problem
    }
}

impl fmt::Display for VmnetInterfaceParameterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "vmnet {} {}", self.field, self.problem)
    }
}

impl std::error::Error for VmnetInterfaceParameterError {}

#[derive(Clone, PartialEq, Eq)]
pub struct VmnetInterfaceParameters {
    realized_mac: GuestMacAddress,
    effective_mtu: u16,
    maximum_packet_size: usize,
    interface_id: Option<[u8; VMNET_INTERFACE_ID_LEN]>,
    read_max_packets: Option<u16>,
    write_max_packets: Option<u16>,
    direct_virtio_header_available: bool,
    direct_virtio_header_enabled: bool,
}

impl VmnetInterfaceParameters {
    #[cfg(test)]
    pub(crate) const fn for_test(
        realized_mac: GuestMacAddress,
        effective_mtu: u16,
        maximum_packet_size: usize,
    ) -> Self {
        Self {
            realized_mac,
            effective_mtu,
            maximum_packet_size,
            interface_id: None,
            read_max_packets: None,
            write_max_packets: None,
            direct_virtio_header_available: false,
            direct_virtio_header_enabled: false,
        }
    }

    pub const fn realized_mac(&self) -> GuestMacAddress {
        self.realized_mac
    }

    pub const fn effective_mtu(&self) -> u16 {
        self.effective_mtu
    }

    pub const fn maximum_packet_size(&self) -> usize {
        self.maximum_packet_size
    }

    pub const fn interface_id(&self) -> Option<[u8; VMNET_INTERFACE_ID_LEN]> {
        self.interface_id
    }

    pub const fn read_max_packets(&self) -> Option<u16> {
        self.read_max_packets
    }

    pub const fn write_max_packets(&self) -> Option<u16> {
        self.write_max_packets
    }

    pub const fn direct_virtio_header_available(&self) -> bool {
        self.direct_virtio_header_available
    }

    pub const fn direct_virtio_header_enabled(&self) -> bool {
        self.direct_virtio_header_enabled
    }

    pub fn packet_buffer_size(&self) -> Option<usize> {
        self.maximum_packet_size
            .checked_add(if self.direct_virtio_header_enabled {
                bangbang_runtime::network::VIRTIO_NET_TX_HEADER_SIZE as usize
            } else {
                0
            })
    }
}

impl fmt::Debug for VmnetInterfaceParameters {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetInterfaceParameters")
            .field("realized_mac", &"<redacted>")
            .field("effective_mtu", &"<redacted>")
            .field("maximum_packet_size", &"<redacted>")
            .field(
                "interface_id",
                &self.interface_id.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "read_max_packets",
                &self.read_max_packets.map(|_| "<redacted>"),
            )
            .field(
                "write_max_packets",
                &self.write_max_packets.map(|_| "<redacted>"),
            )
            .field(
                "direct_virtio_header_available",
                &self.direct_virtio_header_available,
            )
            .field(
                "direct_virtio_header_enabled",
                &self.direct_virtio_header_enabled,
            )
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetInterfaceStartDisposition {
    Retryable,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmnetInterfaceStartError {
    Descriptor {
        source: VmnetInterfaceDescriptorError,
    },
    Start {
        source: VmnetError,
        disposition: VmnetInterfaceStartDisposition,
    },
    Parameters {
        source: VmnetInterfaceParameterError,
        disposition: VmnetInterfaceStartDisposition,
    },
}

impl VmnetInterfaceStartError {
    const fn start(source: VmnetError, disposition: VmnetInterfaceStartDisposition) -> Self {
        Self::Start {
            source,
            disposition,
        }
    }

    const fn parameters(
        source: VmnetInterfaceParameterError,
        disposition: VmnetInterfaceStartDisposition,
    ) -> Self {
        Self::Parameters {
            source,
            disposition,
        }
    }

    pub const fn disposition(&self) -> VmnetInterfaceStartDisposition {
        match self {
            Self::Descriptor { .. } => VmnetInterfaceStartDisposition::Retryable,
            Self::Start { disposition, .. } | Self::Parameters { disposition, .. } => *disposition,
        }
    }
}

impl fmt::Display for VmnetInterfaceStartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Descriptor { source } => {
                write!(f, "failed to build vmnet interface descriptor: {source}")
            }
            Self::Start {
                source,
                disposition,
            } => {
                write!(f, "{source}")?;
                if *disposition == VmnetInterfaceStartDisposition::Terminal {
                    f.write_str("; vmnet cleanup could not be confirmed")?;
                }
                Ok(())
            }
            Self::Parameters {
                source,
                disposition,
            } => {
                write!(f, "failed to validate vmnet interface parameters: {source}")?;
                if *disposition == VmnetInterfaceStartDisposition::Terminal {
                    f.write_str("; vmnet cleanup could not be confirmed")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for VmnetInterfaceStartError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Descriptor { source } => Some(source),
            Self::Start { source, .. } => Some(source),
            Self::Parameters { source, .. } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetPacketDescriptorError {
    EmptyPacketBuffer,
}

impl fmt::Display for VmnetPacketDescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyPacketBuffer => {
                f.write_str("vmnet packet descriptor buffer must not be empty")
            }
        }
    }
}

impl std::error::Error for VmnetPacketDescriptorError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetPacketCountExpectation {
    ZeroOrOne,
    One,
    AtMost(usize),
}

impl fmt::Display for VmnetPacketCountExpectation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroOrOne => f.write_str("0 or 1"),
            Self::One => f.write_str("1"),
            Self::AtMost(maximum) => write!(f, "0 through {maximum}"),
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum VmnetPacketIoError {
    Vmnet {
        source: VmnetError,
    },
    InterfaceStopped,
    UnexpectedPacketCount {
        operation: VmnetOperation,
        expected: VmnetPacketCountExpectation,
        actual: i32,
    },
    ReadPacketSizeExceedsBuffer {
        packet_size: usize,
        buffer_len: usize,
    },
    InvalidBatch {
        message: &'static str,
    },
}

impl VmnetPacketIoError {
    const fn vmnet(operation: VmnetOperation, status: VmnetStatus) -> Self {
        Self::Vmnet {
            source: VmnetError::new(operation, status),
        }
    }
}

impl fmt::Display for VmnetPacketIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vmnet { source } => write!(f, "{source}"),
            Self::InterfaceStopped => f.write_str("vmnet interface is not started"),
            Self::UnexpectedPacketCount {
                operation,
                expected: _,
                actual: _,
            } => write!(f, "{operation} returned an unexpected packet count"),
            Self::ReadPacketSizeExceedsBuffer { .. } => {
                f.write_str("vmnet_read returned a packet larger than the validated read buffer")
            }
            Self::InvalidBatch { message } => write!(f, "invalid vmnet packet batch: {message}"),
        }
    }
}

impl fmt::Debug for VmnetPacketIoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

impl std::error::Error for VmnetPacketIoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Vmnet { source } => Some(source),
            Self::InterfaceStopped
            | Self::UnexpectedPacketCount { .. }
            | Self::ReadPacketSizeExceedsBuffer { .. }
            | Self::InvalidBatch { .. } => None,
        }
    }
}

/// Restricted publisher installed in Apple's packet-available callback.
///
/// The callback receives no interface handle or guest/runtime state. Its only
/// capability is publishing a best-effort packet-count hint to the generation
/// owner supplied by the caller.
#[derive(Clone)]
pub struct VmnetPacketAvailableCallback {
    publish: Arc<dyn Fn(Option<u64>) + Send + Sync + 'static>,
}

impl VmnetPacketAvailableCallback {
    pub fn new(publish: impl Fn(Option<u64>) + Send + Sync + 'static) -> Self {
        Self {
            publish: Arc::new(publish),
        }
    }

    fn publish(&self, estimated_packets: Option<u64>) {
        (self.publish)(estimated_packets);
    }

    #[cfg(test)]
    pub(crate) fn publish_for_test(&self, estimated_packets: Option<u64>) {
        self.publish(estimated_packets);
    }
}

impl fmt::Debug for VmnetPacketAvailableCallback {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VmnetPacketAvailableCallback(<restricted>)")
    }
}

#[derive(Clone, Copy)]
struct VmnetInterfaceResultPolicy {
    mode: VmnetMode,
    requested_mac: Option<GuestMacAddress>,
    requested_mtu: Option<u16>,
    direct_virtio_header_available: bool,
    direct_virtio_header_enabled: bool,
}

impl fmt::Debug for VmnetInterfaceResultPolicy {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetInterfaceResultPolicy")
            .field("mode", &self.mode)
            .field("requested_mac", &self.requested_mac.map(|_| "<configured>"))
            .field("requested_mtu", &self.requested_mtu.map(|_| "<configured>"))
            .field(
                "direct_virtio_header_available",
                &self.direct_virtio_header_available,
            )
            .field(
                "direct_virtio_header_enabled",
                &self.direct_virtio_header_enabled,
            )
            .finish()
    }
}

pub struct VmnetInterfaceDescriptor {
    dictionary: OwnedXpcObject,
    result_policy: VmnetInterfaceResultPolicy,
}

impl VmnetInterfaceDescriptor {
    pub fn new(config: &VmnetInterfaceConfig) -> Result<Self, VmnetInterfaceDescriptorError> {
        let dictionary = OwnedXpcObject::dictionary()
            .ok_or(VmnetInterfaceDescriptorError::CreateDictionaryFailed)?;
        let operation_mode_key = vmnet_operation_mode_key()?;

        // SAFETY: `dictionary` owns a live XPC dictionary, `operation_mode_key`
        // is a non-null vmnet SDK key, and primitive uint64 insertion does not
        // borrow data after the call returns.
        unsafe {
            xpc::xpc_dictionary_set_uint64(
                dictionary.as_ptr(),
                operation_mode_key,
                u64::from(config.mode().raw_value()),
            );
        }

        if let Some(interface_name) = config.bridged_interface_name() {
            let interface_name = CString::new(interface_name)
                .map_err(|_| VmnetInterfaceDescriptorError::InteriorNulInBridgedInterfaceName)?;
            let shared_interface_name_key = vmnet_shared_interface_name_key()?;

            // SAFETY: `dictionary` owns a live XPC dictionary,
            // `shared_interface_name_key` is a non-null vmnet SDK key, and the
            // bridged interface name is a valid C string for the duration of the call.
            unsafe {
                xpc::xpc_dictionary_set_string(
                    dictionary.as_ptr(),
                    shared_interface_name_key,
                    interface_name.as_ptr(),
                );
            }
        }

        let allocate_mac_address_key = vmnet_allocate_mac_address_key()?;
        let allocate_mac_address = config.guest_mac().is_none();
        // SAFETY: `dictionary` owns a live XPC dictionary and the key is a
        // non-null vmnet SDK key. Primitive Boolean insertion does not borrow
        // data after this call.
        unsafe {
            xpc::xpc_dictionary_set_bool(
                dictionary.as_ptr(),
                allocate_mac_address_key,
                allocate_mac_address,
            );
        }

        if let Some(guest_mac) = config.guest_mac() {
            let guest_mac = CString::new(guest_mac.to_string())
                .map_err(|_| VmnetInterfaceDescriptorError::InteriorNulInMacAddress)?;
            let mac_address_key = vmnet_mac_address_key()?;
            // SAFETY: `dictionary` owns a live XPC dictionary, the key is
            // non-null, and `guest_mac` is a valid C string for the call.
            unsafe {
                xpc::xpc_dictionary_set_string(
                    dictionary.as_ptr(),
                    mac_address_key,
                    guest_mac.as_ptr(),
                );
            }
        }

        if config.mode() != VmnetMode::Bridged
            && let Some(mtu) = config.mtu()
        {
            let mtu_key = vmnet_mtu_key()?;
            // SAFETY: `dictionary` owns a live XPC dictionary and the key is a
            // non-null vmnet SDK key. Primitive integer insertion does not
            // borrow data after this call.
            unsafe {
                xpc::xpc_dictionary_set_uint64(dictionary.as_ptr(), mtu_key, u64::from(mtu));
            }
        }

        let direct_virtio_header_key = optional_vmnet_enable_virtio_header_key();
        if let Some(key) = direct_virtio_header_key {
            // SAFETY: `dictionary` owns a live XPC dictionary and `key` was
            // resolved to a non-null vmnet data-symbol value.
            unsafe {
                xpc::xpc_dictionary_set_bool(dictionary.as_ptr(), key, true);
            }
        }

        Ok(Self {
            dictionary,
            result_policy: VmnetInterfaceResultPolicy {
                mode: config.mode(),
                requested_mac: config.guest_mac(),
                requested_mtu: config.mtu(),
                direct_virtio_header_available: direct_virtio_header_key.is_some(),
                direct_virtio_header_enabled: direct_virtio_header_key.is_some(),
            },
        })
    }

    pub fn as_raw_xpc_object(&self) -> *mut c_void {
        self.dictionary.as_ptr()
    }
}

impl fmt::Debug for VmnetInterfaceDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetInterfaceDescriptor")
            .field("dictionary", &"<owned>")
            .field("result_policy", &self.result_policy)
            .finish()
    }
}

#[repr(C)]
pub struct VmnetPacketDescriptor {
    vm_pkt_size: usize,
    vm_pkt_iov: *mut libc::iovec,
    vm_pkt_iovcnt: u32,
    vm_flags: u32,
}

impl VmnetPacketDescriptor {
    pub const fn packet_size(&self) -> usize {
        self.vm_pkt_size
    }

    pub const fn iov_count(&self) -> u32 {
        self.vm_pkt_iovcnt
    }

    pub const fn flags(&self) -> u32 {
        self.vm_flags
    }

    pub const fn iov_ptr(&self) -> *mut libc::iovec {
        self.vm_pkt_iov
    }
}

impl fmt::Debug for VmnetPacketDescriptor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VmnetPacketDescriptor(<borrowed>)")
    }
}

pub struct VmnetWritePacket<'a> {
    descriptor: VmnetPacketDescriptor,
    iov: Box<libc::iovec>,
    _packet: PhantomData<&'a [u8]>,
}

impl fmt::Debug for VmnetWritePacket<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VmnetWritePacket(<borrowed>)")
    }
}

impl<'a> VmnetWritePacket<'a> {
    pub fn new(packet: &'a [u8]) -> Result<Self, VmnetPacketDescriptorError> {
        if packet.is_empty() {
            return Err(VmnetPacketDescriptorError::EmptyPacketBuffer);
        }

        let mut iov = Box::new(libc::iovec {
            iov_base: packet.as_ptr().cast::<c_void>().cast_mut(),
            iov_len: packet.len(),
        });
        let descriptor = VmnetPacketDescriptor {
            vm_pkt_size: packet.len(),
            vm_pkt_iov: ptr::from_mut(iov.as_mut()),
            vm_pkt_iovcnt: 1,
            vm_flags: 0,
        };

        Ok(Self {
            descriptor,
            iov,
            _packet: PhantomData,
        })
    }

    pub const fn as_raw_descriptor(&self) -> &VmnetPacketDescriptor {
        &self.descriptor
    }

    pub fn as_mut_raw_descriptor(&mut self) -> &mut VmnetPacketDescriptor {
        &mut self.descriptor
    }

    pub fn iov(&self) -> &libc::iovec {
        &self.iov
    }
}

pub struct VmnetReadPacket<'a> {
    descriptor: VmnetPacketDescriptor,
    iov: Box<libc::iovec>,
    _buffer: PhantomData<&'a mut [u8]>,
}

impl fmt::Debug for VmnetReadPacket<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VmnetReadPacket(<borrowed>)")
    }
}

impl<'a> VmnetReadPacket<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Result<Self, VmnetPacketDescriptorError> {
        if buffer.is_empty() {
            return Err(VmnetPacketDescriptorError::EmptyPacketBuffer);
        }

        let mut iov = Box::new(libc::iovec {
            iov_base: buffer.as_mut_ptr().cast::<c_void>(),
            iov_len: buffer.len(),
        });
        let descriptor = VmnetPacketDescriptor {
            vm_pkt_size: buffer.len(),
            vm_pkt_iov: ptr::from_mut(iov.as_mut()),
            vm_pkt_iovcnt: 1,
            vm_flags: 0,
        };

        Ok(Self {
            descriptor,
            iov,
            _buffer: PhantomData,
        })
    }

    pub const fn as_raw_descriptor(&self) -> &VmnetPacketDescriptor {
        &self.descriptor
    }

    pub fn as_mut_raw_descriptor(&mut self) -> &mut VmnetPacketDescriptor {
        &mut self.descriptor
    }

    pub fn iov(&self) -> &libc::iovec {
        &self.iov
    }
}

struct OwnedXpcObject {
    object: NonNull<c_void>,
}

impl OwnedXpcObject {
    fn dictionary() -> Option<Self> {
        // SAFETY: `xpc_dictionary_create` permits null key and value arrays
        // when `count` is zero, creating an empty retained dictionary object.
        let object = unsafe { xpc::xpc_dictionary_create(ptr::null(), ptr::null(), 0) };

        NonNull::new(object).map(|object| Self { object })
    }

    fn as_ptr(&self) -> xpc::XpcObject {
        self.object.as_ptr()
    }
}

impl Drop for OwnedXpcObject {
    fn drop(&mut self) {
        // SAFETY: `object` came from an XPC create function and this owner is
        // non-clone, so this is the single matching release.
        unsafe {
            xpc::xpc_release(self.object.as_ptr());
        }
    }
}

impl fmt::Debug for OwnedXpcObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("OwnedXpcObject(<owned>)")
    }
}

fn vmnet_operation_mode_key() -> Result<*const c_char, VmnetInterfaceDescriptorError> {
    // SAFETY: The symbol is provided by vmnet.framework when this macOS-gated
    // module is linked. The null check below prevents passing a null key to XPC.
    let key = unsafe { xpc::VMNET_OPERATION_MODE_KEY };
    if key.is_null() {
        Err(VmnetInterfaceDescriptorError::MissingVmnetKey(
            "vmnet_operation_mode_key",
        ))
    } else {
        Ok(key)
    }
}

fn vmnet_shared_interface_name_key() -> Result<*const c_char, VmnetInterfaceDescriptorError> {
    // SAFETY: The symbol is provided by vmnet.framework when this macOS-gated
    // module is linked. The null check below prevents passing a null key to XPC.
    let key = unsafe { xpc::VMNET_SHARED_INTERFACE_NAME_KEY };
    if key.is_null() {
        Err(VmnetInterfaceDescriptorError::MissingVmnetKey(
            "vmnet_shared_interface_name_key",
        ))
    } else {
        Ok(key)
    }
}

fn vmnet_mac_address_key() -> Result<*const c_char, VmnetInterfaceDescriptorError> {
    // SAFETY: The symbol is provided by vmnet.framework on every supported
    // deployment target. The null check prevents passing a null key to XPC.
    let key = unsafe { xpc::VMNET_MAC_ADDRESS_KEY };
    if key.is_null() {
        Err(VmnetInterfaceDescriptorError::MissingVmnetKey(
            "vmnet_mac_address_key",
        ))
    } else {
        Ok(key)
    }
}

fn vmnet_allocate_mac_address_key() -> Result<*const c_char, VmnetInterfaceDescriptorError> {
    // SAFETY: The symbol is provided by vmnet.framework on every supported
    // deployment target. The null check prevents passing a null key to XPC.
    let key = unsafe { xpc::VMNET_ALLOCATE_MAC_ADDRESS_KEY };
    if key.is_null() {
        Err(VmnetInterfaceDescriptorError::MissingVmnetKey(
            "vmnet_allocate_mac_address_key",
        ))
    } else {
        Ok(key)
    }
}

fn vmnet_mtu_key() -> Result<*const c_char, VmnetInterfaceDescriptorError> {
    // SAFETY: The symbol is provided by vmnet.framework on every supported
    // deployment target. The null check prevents passing a null key to XPC.
    let key = unsafe { xpc::VMNET_MTU_KEY };
    if key.is_null() {
        Err(VmnetInterfaceDescriptorError::MissingVmnetKey(
            "vmnet_mtu_key",
        ))
    } else {
        Ok(key)
    }
}

fn vmnet_max_packet_size_key() -> Result<*const c_char, VmnetInterfaceDescriptorError> {
    // SAFETY: The symbol is provided by vmnet.framework on every supported
    // deployment target. The null check prevents passing a null key to XPC.
    let key = unsafe { xpc::VMNET_MAX_PACKET_SIZE_KEY };
    if key.is_null() {
        Err(VmnetInterfaceDescriptorError::MissingVmnetKey(
            "vmnet_max_packet_size_key",
        ))
    } else {
        Ok(key)
    }
}

fn vmnet_interface_id_key() -> Result<*const c_char, VmnetInterfaceDescriptorError> {
    // SAFETY: The symbol is provided by vmnet.framework on every supported
    // deployment target. The null check prevents passing a null key to XPC.
    let key = unsafe { xpc::VMNET_INTERFACE_ID_KEY };
    if key.is_null() {
        Err(VmnetInterfaceDescriptorError::MissingVmnetKey(
            "vmnet_interface_id_key",
        ))
    } else {
        Ok(key)
    }
}

fn vmnet_estimated_packets_available_key() -> Option<*const c_char> {
    // SAFETY: The symbol is provided by vmnet.framework on every deployment
    // target that exposes packet events. A null defensive check keeps malformed
    // framework state from reaching XPC dictionary access.
    let key = unsafe { xpc::VMNET_ESTIMATED_PACKETS_AVAILABLE_KEY };
    (!key.is_null()).then_some(key)
}

fn optional_vmnet_read_max_packets_key() -> Option<*const c_char> {
    optional_vmnet_data_key(c"vmnet_read_max_packets_key")
}

fn optional_vmnet_write_max_packets_key() -> Option<*const c_char> {
    optional_vmnet_data_key(c"vmnet_write_max_packets_key")
}

fn optional_vmnet_enable_virtio_header_key() -> Option<*const c_char> {
    optional_vmnet_data_key(c"vmnet_enable_virtio_header_key")
}

fn optional_vmnet_data_key(symbol: &CStr) -> Option<*const c_char> {
    // SAFETY: `symbol` is NUL-terminated. vmnet key exports are data symbols
    // whose address points to one `const char *`; reading that pointer does not
    // transfer ownership. A missing symbol or null key is treated as unavailable.
    let symbol_address = unsafe { libc::dlsym(libc::RTLD_DEFAULT, symbol.as_ptr()) };
    let symbol_address = NonNull::new(symbol_address)?;
    // SAFETY: A successful lookup for a vmnet key data symbol points to storage
    // containing one `const char *` value, as declared by the public SDK.
    let key = unsafe { symbol_address.cast::<*const c_char>().as_ptr().read() };
    (!key.is_null()).then_some(key)
}

mod xpc {
    use std::ffi::{c_char, c_void};

    pub type XpcObject = *mut c_void;

    unsafe extern "C" {
        pub fn xpc_dictionary_create(
            keys: *const *const c_char,
            values: *const XpcObject,
            count: usize,
        ) -> XpcObject;
        pub fn xpc_dictionary_set_uint64(xdict: XpcObject, key: *const c_char, value: u64);
        pub fn xpc_dictionary_set_bool(xdict: XpcObject, key: *const c_char, value: bool);
        pub fn xpc_dictionary_set_string(
            xdict: XpcObject,
            key: *const c_char,
            string: *const c_char,
        );
        #[cfg(test)]
        pub fn xpc_dictionary_set_uuid(xdict: XpcObject, key: *const c_char, uuid: *const u8);
        #[cfg(test)]
        pub fn xpc_dictionary_get_bool(xdict: XpcObject, key: *const c_char) -> bool;
        #[cfg(test)]
        pub fn xpc_dictionary_get_uint64(xdict: XpcObject, key: *const c_char) -> u64;
        #[cfg(test)]
        pub fn xpc_dictionary_get_string(xdict: XpcObject, key: *const c_char) -> *const c_char;
        pub fn xpc_dictionary_get_value(xdict: XpcObject, key: *const c_char) -> XpcObject;
        pub fn xpc_get_type(object: XpcObject) -> *const c_void;
        pub fn xpc_string_get_length(object: XpcObject) -> usize;
        pub fn xpc_string_get_string_ptr(object: XpcObject) -> *const c_char;
        pub fn xpc_uint64_get_value(object: XpcObject) -> u64;
        pub fn xpc_uuid_get_bytes(object: XpcObject) -> *const u8;
        pub fn xpc_release(object: XpcObject);

        #[link_name = "_xpc_type_dictionary"]
        pub static XPC_TYPE_DICTIONARY: c_void;
        #[link_name = "_xpc_type_string"]
        pub static XPC_TYPE_STRING: c_void;
        #[link_name = "_xpc_type_uint64"]
        pub static XPC_TYPE_UINT64: c_void;
        #[link_name = "_xpc_type_uuid"]
        pub static XPC_TYPE_UUID: c_void;
    }

    #[link(name = "vmnet", kind = "framework")]
    unsafe extern "C" {
        #[link_name = "vmnet_operation_mode_key"]
        pub static VMNET_OPERATION_MODE_KEY: *const c_char;
        #[link_name = "vmnet_shared_interface_name_key"]
        pub static VMNET_SHARED_INTERFACE_NAME_KEY: *const c_char;
        #[link_name = "vmnet_mac_address_key"]
        pub static VMNET_MAC_ADDRESS_KEY: *const c_char;
        #[link_name = "vmnet_allocate_mac_address_key"]
        pub static VMNET_ALLOCATE_MAC_ADDRESS_KEY: *const c_char;
        #[link_name = "vmnet_mtu_key"]
        pub static VMNET_MTU_KEY: *const c_char;
        #[link_name = "vmnet_max_packet_size_key"]
        pub static VMNET_MAX_PACKET_SIZE_KEY: *const c_char;
        #[link_name = "vmnet_interface_id_key"]
        pub static VMNET_INTERFACE_ID_KEY: *const c_char;
        #[link_name = "vmnet_estimated_packets_available_key"]
        pub static VMNET_ESTIMATED_PACKETS_AVAILABLE_KEY: *const c_char;
    }
}

#[derive(Clone, PartialEq, Eq)]
struct RawVmnetInterfaceParameters {
    returned_mac: Option<GuestMacAddress>,
    effective_mtu: u64,
    maximum_packet_size: u64,
    interface_id: Option<[u8; VMNET_INTERFACE_ID_LEN]>,
    read_max_packets: Option<u64>,
    write_max_packets: Option<u64>,
}

impl fmt::Debug for RawVmnetInterfaceParameters {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RawVmnetInterfaceParameters")
            .field("returned_mac", &self.returned_mac.map(|_| "<redacted>"))
            .field("effective_mtu", &"<redacted>")
            .field("maximum_packet_size", &"<redacted>")
            .field(
                "interface_id",
                &self.interface_id.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "read_max_packets",
                &self.read_max_packets.map(|_| "<redacted>"),
            )
            .field(
                "write_max_packets",
                &self.write_max_packets.map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl RawVmnetInterfaceParameters {
    fn validate(
        self,
        policy: VmnetInterfaceResultPolicy,
    ) -> Result<VmnetInterfaceParameters, VmnetInterfaceParameterError> {
        let realized_mac = match (policy.requested_mac, self.returned_mac) {
            (Some(requested), Some(returned)) if requested != returned => {
                return Err(VmnetInterfaceParameterError::new(
                    VmnetInterfaceParameterField::MacAddress,
                    VmnetInterfaceParameterProblem::ConflictsWithRequest,
                ));
            }
            (Some(requested), Some(_) | None) => requested,
            (None, Some(returned)) if allocated_mac_is_valid(returned) => returned,
            (None, Some(_)) => {
                return Err(VmnetInterfaceParameterError::new(
                    VmnetInterfaceParameterField::MacAddress,
                    VmnetInterfaceParameterProblem::OutOfRange,
                ));
            }
            (None, None) => {
                return Err(VmnetInterfaceParameterError::new(
                    VmnetInterfaceParameterField::MacAddress,
                    VmnetInterfaceParameterProblem::Missing,
                ));
            }
        };

        let effective_mtu = u16::try_from(self.effective_mtu).map_err(|_| {
            VmnetInterfaceParameterError::new(
                VmnetInterfaceParameterField::EffectiveMtu,
                VmnetInterfaceParameterProblem::OutOfRange,
            )
        })?;
        if !(VIRTIO_NET_MIN_MTU..=VIRTIO_NET_MAX_MTU).contains(&effective_mtu) {
            return Err(VmnetInterfaceParameterError::new(
                VmnetInterfaceParameterField::EffectiveMtu,
                VmnetInterfaceParameterProblem::OutOfRange,
            ));
        }
        match (policy.mode, policy.requested_mtu) {
            (VmnetMode::Host | VmnetMode::Shared, Some(requested))
                if requested != effective_mtu =>
            {
                return Err(VmnetInterfaceParameterError::new(
                    VmnetInterfaceParameterField::EffectiveMtu,
                    VmnetInterfaceParameterProblem::ConflictsWithRequest,
                ));
            }
            (VmnetMode::Bridged, Some(requested)) if requested > effective_mtu => {
                return Err(VmnetInterfaceParameterError::new(
                    VmnetInterfaceParameterField::EffectiveMtu,
                    VmnetInterfaceParameterProblem::ConflictsWithRequest,
                ));
            }
            _ => {}
        }

        if policy.direct_virtio_header_enabled && !policy.direct_virtio_header_available {
            return Err(VmnetInterfaceParameterError::new(
                VmnetInterfaceParameterField::DirectVirtioHeader,
                VmnetInterfaceParameterProblem::ConflictsWithRequest,
            ));
        }
        if self.maximum_packet_size == 0
            || self.maximum_packet_size < u64::from(effective_mtu)
            || self.maximum_packet_size > c_int::MAX as u64
        {
            return Err(VmnetInterfaceParameterError::new(
                VmnetInterfaceParameterField::MaximumPacketSize,
                VmnetInterfaceParameterProblem::OutOfRange,
            ));
        }
        let virtio_header_size = u64::from(bangbang_runtime::network::VIRTIO_NET_TX_HEADER_SIZE);
        let packet_buffer_size = self
            .maximum_packet_size
            .checked_add(if policy.direct_virtio_header_enabled {
                virtio_header_size
            } else {
                0
            })
            .ok_or_else(|| {
                VmnetInterfaceParameterError::new(
                    VmnetInterfaceParameterField::DirectVirtioHeader,
                    VmnetInterfaceParameterProblem::OutOfRange,
                )
            })?;
        let guest_buffer_size = self
            .maximum_packet_size
            .checked_add(virtio_header_size)
            .ok_or_else(|| {
                VmnetInterfaceParameterError::new(
                    if policy.direct_virtio_header_enabled {
                        VmnetInterfaceParameterField::DirectVirtioHeader
                    } else {
                        VmnetInterfaceParameterField::MaximumPacketSize
                    },
                    VmnetInterfaceParameterProblem::OutOfRange,
                )
            })?;
        if guest_buffer_size > VIRTIO_NET_MAX_BUFFER_SIZE {
            return Err(VmnetInterfaceParameterError::new(
                if policy.direct_virtio_header_enabled {
                    VmnetInterfaceParameterField::DirectVirtioHeader
                } else {
                    VmnetInterfaceParameterField::MaximumPacketSize
                },
                VmnetInterfaceParameterProblem::OutOfRange,
            ));
        }
        let maximum_packet_size = usize::try_from(self.maximum_packet_size).map_err(|_| {
            VmnetInterfaceParameterError::new(
                VmnetInterfaceParameterField::MaximumPacketSize,
                VmnetInterfaceParameterProblem::OutOfRange,
            )
        })?;
        let read_max_packets = validate_batch_limit(
            self.read_max_packets,
            VmnetInterfaceParameterField::ReadMaximumPackets,
            packet_buffer_size,
        )?;
        let write_max_packets = validate_batch_limit(
            self.write_max_packets,
            VmnetInterfaceParameterField::WriteMaximumPackets,
            packet_buffer_size,
        )?;

        Ok(VmnetInterfaceParameters {
            realized_mac,
            effective_mtu,
            maximum_packet_size,
            interface_id: self.interface_id,
            read_max_packets,
            write_max_packets,
            direct_virtio_header_available: policy.direct_virtio_header_available,
            direct_virtio_header_enabled: policy.direct_virtio_header_enabled,
        })
    }
}

fn allocated_mac_is_valid(mac: GuestMacAddress) -> bool {
    let octets = mac.octets();
    octets != [0; 6] && octets[0] & 1 == 0
}

fn validate_batch_limit(
    returned: Option<u64>,
    field: VmnetInterfaceParameterField,
    packet_buffer_size: u64,
) -> Result<Option<u16>, VmnetInterfaceParameterError> {
    let Some(returned) = returned else {
        return Ok(None);
    };
    if returned == 0 || returned > c_int::MAX as u64 {
        return Err(VmnetInterfaceParameterError::new(
            field,
            VmnetInterfaceParameterProblem::OutOfRange,
        ));
    }
    let memory_limit = VMNET_MAX_BYTES_PER_OPERATION as u64 / packet_buffer_size;
    let capped = returned
        .min(VMNET_MAX_PACKETS_PER_OPERATION as u64)
        .min(u64::from(VIRTIO_NET_QUEUE_SIZE))
        .min(memory_limit);
    let capped = u16::try_from(capped).map_err(|_| {
        VmnetInterfaceParameterError::new(field, VmnetInterfaceParameterProblem::OutOfRange)
    })?;
    if capped == 0 {
        return Err(VmnetInterfaceParameterError::new(
            field,
            VmnetInterfaceParameterProblem::OutOfRange,
        ));
    }
    Ok(Some(capped))
}

fn decode_vmnet_interface_parameters(
    dictionary: xpc::XpcObject,
) -> Result<RawVmnetInterfaceParameters, VmnetInterfaceParameterError> {
    if dictionary.is_null() || !xpc_object_has_type(dictionary, xpc_dictionary_type()) {
        return Err(VmnetInterfaceParameterError::new(
            VmnetInterfaceParameterField::ResultDictionary,
            if dictionary.is_null() {
                VmnetInterfaceParameterProblem::Missing
            } else {
                VmnetInterfaceParameterProblem::WrongType
            },
        ));
    }

    let returned_mac = optional_xpc_mac(
        dictionary,
        required_result_key(
            vmnet_mac_address_key(),
            VmnetInterfaceParameterField::MacAddress,
        )?,
    )?;
    let effective_mtu = required_xpc_uint64(
        dictionary,
        required_result_key(vmnet_mtu_key(), VmnetInterfaceParameterField::EffectiveMtu)?,
        VmnetInterfaceParameterField::EffectiveMtu,
    )?;
    let maximum_packet_size = required_xpc_uint64(
        dictionary,
        required_result_key(
            vmnet_max_packet_size_key(),
            VmnetInterfaceParameterField::MaximumPacketSize,
        )?,
        VmnetInterfaceParameterField::MaximumPacketSize,
    )?;
    let interface_id = optional_xpc_uuid(
        dictionary,
        required_result_key(
            vmnet_interface_id_key(),
            VmnetInterfaceParameterField::InterfaceId,
        )?,
        VmnetInterfaceParameterField::InterfaceId,
    )?;
    let read_max_packets = optional_vmnet_read_max_packets_key()
        .map(|key| {
            optional_xpc_uint64(
                dictionary,
                key,
                VmnetInterfaceParameterField::ReadMaximumPackets,
            )
        })
        .transpose()?
        .flatten();
    let write_max_packets = optional_vmnet_write_max_packets_key()
        .map(|key| {
            optional_xpc_uint64(
                dictionary,
                key,
                VmnetInterfaceParameterField::WriteMaximumPackets,
            )
        })
        .transpose()?
        .flatten();

    Ok(RawVmnetInterfaceParameters {
        returned_mac,
        effective_mtu,
        maximum_packet_size,
        interface_id,
        read_max_packets,
        write_max_packets,
    })
}

fn required_result_key(
    key: Result<*const c_char, VmnetInterfaceDescriptorError>,
    field: VmnetInterfaceParameterField,
) -> Result<*const c_char, VmnetInterfaceParameterError> {
    key.map_err(|_| {
        VmnetInterfaceParameterError::new(field, VmnetInterfaceParameterProblem::Missing)
    })
}

fn optional_xpc_mac(
    dictionary: xpc::XpcObject,
    key: *const c_char,
) -> Result<Option<GuestMacAddress>, VmnetInterfaceParameterError> {
    let field = VmnetInterfaceParameterField::MacAddress;
    let Some(object) = dictionary_value(dictionary, key) else {
        return Ok(None);
    };
    if !xpc_object_has_type(object, xpc_string_type()) {
        return Err(VmnetInterfaceParameterError::new(
            field,
            VmnetInterfaceParameterProblem::WrongType,
        ));
    }
    // SAFETY: Exact XPC string type was established above and the object stays
    // borrowed from the live callback dictionary throughout these calls.
    let len = unsafe { xpc::xpc_string_get_length(object) };
    if len != VMNET_MAC_ADDRESS_STRING_LEN {
        return Err(VmnetInterfaceParameterError::new(
            field,
            VmnetInterfaceParameterProblem::Malformed,
        ));
    }
    // SAFETY: Exact XPC string type was established above. XPC returns a
    // NUL-terminated pointer valid while the borrowed object is alive.
    let value = unsafe { xpc::xpc_string_get_string_ptr(object) };
    if value.is_null() {
        return Err(VmnetInterfaceParameterError::new(
            field,
            VmnetInterfaceParameterProblem::Malformed,
        ));
    }
    // SAFETY: `value` is a non-null NUL-terminated XPC string pointer and is
    // consumed only during this callback-bound decode.
    let value = unsafe { CStr::from_ptr(value) };
    if value.to_bytes().len() != len {
        return Err(VmnetInterfaceParameterError::new(
            field,
            VmnetInterfaceParameterProblem::Malformed,
        ));
    }
    let value = value.to_str().map_err(|_| {
        VmnetInterfaceParameterError::new(field, VmnetInterfaceParameterProblem::Malformed)
    })?;
    GuestMacAddress::from_str(value).map(Some).map_err(|_| {
        VmnetInterfaceParameterError::new(field, VmnetInterfaceParameterProblem::Malformed)
    })
}

fn required_xpc_uint64(
    dictionary: xpc::XpcObject,
    key: *const c_char,
    field: VmnetInterfaceParameterField,
) -> Result<u64, VmnetInterfaceParameterError> {
    optional_xpc_uint64(dictionary, key, field)?.ok_or_else(|| {
        VmnetInterfaceParameterError::new(field, VmnetInterfaceParameterProblem::Missing)
    })
}

fn optional_xpc_uint64(
    dictionary: xpc::XpcObject,
    key: *const c_char,
    field: VmnetInterfaceParameterField,
) -> Result<Option<u64>, VmnetInterfaceParameterError> {
    let Some(object) = dictionary_value(dictionary, key) else {
        return Ok(None);
    };
    if !xpc_object_has_type(object, xpc_uint64_type()) {
        return Err(VmnetInterfaceParameterError::new(
            field,
            VmnetInterfaceParameterProblem::WrongType,
        ));
    }
    // SAFETY: Exact XPC uint64 type was established and the object remains
    // borrowed from the live callback dictionary for this synchronous call.
    Ok(Some(unsafe { xpc::xpc_uint64_get_value(object) }))
}

fn optional_xpc_uuid(
    dictionary: xpc::XpcObject,
    key: *const c_char,
    field: VmnetInterfaceParameterField,
) -> Result<Option<[u8; VMNET_INTERFACE_ID_LEN]>, VmnetInterfaceParameterError> {
    let Some(object) = dictionary_value(dictionary, key) else {
        return Ok(None);
    };
    if !xpc_object_has_type(object, xpc_uuid_type()) {
        return Err(VmnetInterfaceParameterError::new(
            field,
            VmnetInterfaceParameterProblem::WrongType,
        ));
    }
    // SAFETY: Exact XPC UUID type was established. The returned 16-byte pointer
    // remains valid while the callback dictionary owns the object.
    let bytes = unsafe { xpc::xpc_uuid_get_bytes(object) };
    if bytes.is_null() {
        return Err(VmnetInterfaceParameterError::new(
            field,
            VmnetInterfaceParameterProblem::Malformed,
        ));
    }
    let mut copied = [0_u8; VMNET_INTERFACE_ID_LEN];
    // SAFETY: `bytes` points to the public XPC UUID object's fixed 16-byte
    // payload and `copied` has exactly that length without overlap.
    unsafe {
        ptr::copy_nonoverlapping(bytes, copied.as_mut_ptr(), copied.len());
    }
    if copied == [0; VMNET_INTERFACE_ID_LEN] {
        return Err(VmnetInterfaceParameterError::new(
            field,
            VmnetInterfaceParameterProblem::OutOfRange,
        ));
    }
    Ok(Some(copied))
}

fn dictionary_value(dictionary: xpc::XpcObject, key: *const c_char) -> Option<xpc::XpcObject> {
    // SAFETY: `dictionary` was verified as an XPC dictionary and `key` is a
    // non-null vmnet SDK key. The returned object remains borrowed.
    NonNull::new(unsafe { xpc::xpc_dictionary_get_value(dictionary, key) }).map(NonNull::as_ptr)
}

fn decode_vmnet_packet_estimate(event: xpc::XpcObject) -> Option<u64> {
    if event.is_null() || !xpc_object_has_type(event, xpc_dictionary_type()) {
        return None;
    }
    let value = dictionary_value(event, vmnet_estimated_packets_available_key()?)?;
    if !xpc_object_has_type(value, xpc_uint64_type()) {
        return None;
    }

    // SAFETY: Exact XPC uint64 type was established above and `value` remains
    // borrowed from the live event dictionary for this callback invocation.
    Some(unsafe { xpc::xpc_uint64_get_value(value) })
}

fn xpc_object_has_type(object: xpc::XpcObject, expected: *const c_void) -> bool {
    // SAFETY: `object` is non-null and borrowed from a live XPC dictionary.
    unsafe { xpc::xpc_get_type(object) == expected }
}

fn xpc_dictionary_type() -> *const c_void {
    ptr::addr_of!(xpc::XPC_TYPE_DICTIONARY).cast()
}

fn xpc_string_type() -> *const c_void {
    ptr::addr_of!(xpc::XPC_TYPE_STRING).cast()
}

fn xpc_uint64_type() -> *const c_void {
    ptr::addr_of!(xpc::XPC_TYPE_UINT64).cast()
}

fn xpc_uuid_type() -> *const c_void {
    ptr::addr_of!(xpc::XPC_TYPE_UUID).cast()
}

mod vmnet_sys {
    use std::ffi::{c_int, c_void};

    use block2::Block;
    use dispatch2::DispatchQueue;

    use super::VmnetPacketDescriptor;

    #[link(name = "vmnet", kind = "framework")]
    unsafe extern "C" {
        pub fn vmnet_start_interface(
            interface_desc: *mut c_void,
            queue: &DispatchQueue,
            handler: &Block<dyn Fn(u32, *mut c_void)>,
        ) -> *mut c_void;
        pub fn vmnet_stop_interface(
            interface: *mut c_void,
            queue: &DispatchQueue,
            handler: &Block<dyn Fn(u32)>,
        ) -> u32;
        pub fn vmnet_read(
            interface: *mut c_void,
            packets: *mut VmnetPacketDescriptor,
            packet_count: *mut c_int,
        ) -> u32;
        pub fn vmnet_write(
            interface: *mut c_void,
            packets: *mut VmnetPacketDescriptor,
            packet_count: *mut c_int,
        ) -> u32;
        pub fn vmnet_interface_set_event_callback(
            interface: *mut c_void,
            event_mask: u32,
            queue: Option<&DispatchQueue>,
            callback: Option<&Block<dyn Fn(u32, *mut c_void)>>,
        ) -> u32;
    }
}

pub struct VmnetStartedInterface<I> {
    interface: I,
    parameters: VmnetInterfaceParameters,
}

impl<I> VmnetStartedInterface<I> {
    pub const fn new(interface: I, parameters: VmnetInterfaceParameters) -> Self {
        Self {
            interface,
            parameters,
        }
    }

    pub const fn parameters(&self) -> &VmnetInterfaceParameters {
        &self.parameters
    }

    pub fn into_parts(self) -> (I, VmnetInterfaceParameters) {
        (self.interface, self.parameters)
    }
}

impl<I> fmt::Debug for VmnetStartedInterface<I> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetStartedInterface")
            .field("interface", &"<owned>")
            .field("parameters", &self.parameters)
            .finish()
    }
}

pub trait VmnetInterfaceBackend: fmt::Debug + Send + 'static {
    type Interface: fmt::Debug + Send + 'static;

    fn build_interface_descriptor(
        &mut self,
        config: &VmnetInterfaceConfig,
    ) -> Result<VmnetInterfaceDescriptor, VmnetInterfaceDescriptorError> {
        VmnetInterfaceDescriptor::new(config)
    }

    fn start_interface(
        &mut self,
        descriptor: &VmnetInterfaceDescriptor,
    ) -> Result<VmnetStartedInterface<Self::Interface>, VmnetInterfaceStartError>;

    fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError>;

    fn enable_packet_available_callback(
        &mut self,
        interface: &mut Self::Interface,
        callback: VmnetPacketAvailableCallback,
    ) -> Result<(), VmnetError>;

    fn disable_and_drain_packet_available_callback(
        &mut self,
        interface: &mut Self::Interface,
    ) -> Result<(), VmnetError>;
}

pub trait VmnetPacketIoBackend: fmt::Debug + Send + 'static {
    type Interface: fmt::Debug + Send + 'static;

    fn read_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetReadPacket<'_>,
    ) -> Result<Option<usize>, VmnetPacketIoError>;

    fn write_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetWritePacket<'_>,
    ) -> Result<(), VmnetPacketIoError>;

    /// Reads at most `requested_packets` fixed-capacity packets into one
    /// contiguous owned buffer and returns the initialized prefix length.
    ///
    /// Backends overriding this method must fill `packet_lengths[..count]`
    /// only and must never expose a count greater than the request.
    fn read_packet_batch(
        &mut self,
        interface: &mut Self::Interface,
        buffer: &mut [u8],
        packet_capacity: usize,
        requested_packets: usize,
        packet_lengths: &mut [usize],
    ) -> Result<usize, VmnetPacketIoError> {
        validate_read_batch_layout(
            buffer.len(),
            packet_capacity,
            requested_packets,
            packet_lengths.len(),
        )?;
        let mut completed = 0;
        for (packet_index, packet_len_slot) in packet_lengths
            .iter_mut()
            .take(requested_packets)
            .enumerate()
        {
            let start = packet_index.checked_mul(packet_capacity).ok_or(
                VmnetPacketIoError::InvalidBatch {
                    message: "read packet offset overflowed",
                },
            )?;
            let end =
                start
                    .checked_add(packet_capacity)
                    .ok_or(VmnetPacketIoError::InvalidBatch {
                        message: "read packet range overflowed",
                    })?;
            let packet_buffer =
                buffer
                    .get_mut(start..end)
                    .ok_or(VmnetPacketIoError::InvalidBatch {
                        message: "read packet range exceeds the aggregate buffer",
                    })?;
            let mut packet = VmnetReadPacket::new(packet_buffer).map_err(|_| {
                VmnetPacketIoError::InvalidBatch {
                    message: "read packet buffer must not be empty",
                }
            })?;
            let Some(packet_len) = self.read_packet(interface, &mut packet)? else {
                break;
            };
            *packet_len_slot = packet_len;
            completed += 1;
        }
        Ok(completed)
    }

    /// Writes a bounded list of packet ranges from one sink-owned staging
    /// buffer and returns the successfully written prefix length.
    fn write_packet_batch(
        &mut self,
        interface: &mut Self::Interface,
        buffer: &[u8],
        packet_ranges: &[Range<usize>],
    ) -> Result<usize, VmnetPacketIoError> {
        validate_write_batch_layout(buffer, packet_ranges)?;
        let mut completed = 0;
        for range in packet_ranges {
            let bytes = buffer
                .get(range.clone())
                .ok_or(VmnetPacketIoError::InvalidBatch {
                    message: "write packet range exceeds the aggregate buffer",
                })?;
            let mut packet =
                VmnetWritePacket::new(bytes).map_err(|_| VmnetPacketIoError::InvalidBatch {
                    message: "write packet buffer must not be empty",
                })?;
            self.write_packet(interface, &mut packet)?;
            completed += 1;
        }
        Ok(completed)
    }
}

fn validate_read_batch_layout(
    buffer_len: usize,
    packet_capacity: usize,
    requested_packets: usize,
    packet_lengths_len: usize,
) -> Result<(), VmnetPacketIoError> {
    if packet_capacity == 0 {
        return Err(VmnetPacketIoError::InvalidBatch {
            message: "read packet capacity must not be zero",
        });
    }
    if requested_packets == 0 || requested_packets > VMNET_MAX_PACKETS_PER_OPERATION {
        return Err(VmnetPacketIoError::InvalidBatch {
            message: "read packet count is outside the vmnet operation bound",
        });
    }
    if packet_lengths_len < requested_packets {
        return Err(VmnetPacketIoError::InvalidBatch {
            message: "read packet length storage is smaller than the request",
        });
    }
    let required =
        packet_capacity
            .checked_mul(requested_packets)
            .ok_or(VmnetPacketIoError::InvalidBatch {
                message: "read aggregate byte count overflowed",
            })?;
    if required > buffer_len || required > VMNET_MAX_BYTES_PER_OPERATION {
        return Err(VmnetPacketIoError::InvalidBatch {
            message: "read aggregate buffer exceeds its validated byte bound",
        });
    }
    Ok(())
}

fn validate_write_batch_layout(
    buffer: &[u8],
    packet_ranges: &[Range<usize>],
) -> Result<(), VmnetPacketIoError> {
    if packet_ranges.is_empty() || packet_ranges.len() > VMNET_MAX_PACKETS_PER_OPERATION {
        return Err(VmnetPacketIoError::InvalidBatch {
            message: "write packet count is outside the vmnet operation bound",
        });
    }
    if buffer.len() > VMNET_MAX_BYTES_PER_OPERATION {
        return Err(VmnetPacketIoError::InvalidBatch {
            message: "write aggregate buffer exceeds the vmnet byte bound",
        });
    }
    let mut previous_end = 0;
    for range in packet_ranges {
        if range.is_empty() || range.start < previous_end || range.end > buffer.len() {
            return Err(VmnetPacketIoError::InvalidBatch {
                message: "write packet ranges are empty, overlapping, or out of bounds",
            });
        }
        previous_end = range.end;
    }
    Ok(())
}

trait VmnetSystemApi: fmt::Debug + Send + 'static {
    fn start_interface(
        &mut self,
        descriptor: &VmnetInterfaceDescriptor,
        queue: &DispatchQueue,
        completion: &Block<dyn Fn(u32, *mut c_void)>,
    ) -> Option<NonNull<c_void>>;

    fn stop_interface(
        &mut self,
        interface: NonNull<c_void>,
        queue: &DispatchQueue,
        completion: &Block<dyn Fn(u32)>,
    ) -> VmnetStatus;

    fn set_event_callback(
        &mut self,
        interface: NonNull<c_void>,
        event_mask: u32,
        queue: Option<&DispatchQueue>,
        callback: Option<&Block<dyn Fn(u32, *mut c_void)>>,
    ) -> VmnetStatus;

    fn read_packets(
        &mut self,
        interface: NonNull<c_void>,
        packets: NonNull<VmnetPacketDescriptor>,
        packet_count: &mut c_int,
    ) -> VmnetStatus;

    fn write_packets(
        &mut self,
        interface: NonNull<c_void>,
        packets: NonNull<VmnetPacketDescriptor>,
        packet_count: &mut c_int,
    ) -> VmnetStatus;
}

#[derive(Debug, Default)]
struct SystemVmnetApi;

impl VmnetSystemApi for SystemVmnetApi {
    fn start_interface(
        &mut self,
        descriptor: &VmnetInterfaceDescriptor,
        queue: &DispatchQueue,
        completion: &Block<dyn Fn(u32, *mut c_void)>,
    ) -> Option<NonNull<c_void>> {
        // SAFETY: `descriptor` owns a live XPC dictionary, `queue` is a live
        // dispatch queue, and `completion` is a valid Objective-C block. The
        // vmnet async completion API owns any later callback use by copying the
        // block before returning.
        let interface = unsafe {
            vmnet_sys::vmnet_start_interface(descriptor.as_raw_xpc_object(), queue, completion)
        };

        NonNull::new(interface)
    }

    fn stop_interface(
        &mut self,
        interface: NonNull<c_void>,
        queue: &DispatchQueue,
        completion: &Block<dyn Fn(u32)>,
    ) -> VmnetStatus {
        // SAFETY: `interface` is an opaque handle returned by
        // `vmnet_start_interface`, `queue` is a live dispatch queue, and
        // `completion` is a valid Objective-C block. The vmnet async completion
        // API owns any later callback use by copying the block before returning.
        let status =
            unsafe { vmnet_sys::vmnet_stop_interface(interface.as_ptr(), queue, completion) };

        VmnetStatus::from_raw(status)
    }

    fn set_event_callback(
        &mut self,
        interface: NonNull<c_void>,
        event_mask: u32,
        queue: Option<&DispatchQueue>,
        callback: Option<&Block<dyn Fn(u32, *mut c_void)>>,
    ) -> VmnetStatus {
        // SAFETY: `interface` is a live vmnet handle. A non-null queue and
        // callback are paired for enable; both are null for disable. vmnet
        // copies the callback before returning from a successful enable.
        let status = unsafe {
            vmnet_sys::vmnet_interface_set_event_callback(
                interface.as_ptr(),
                event_mask,
                queue,
                callback,
            )
        };
        VmnetStatus::from_raw(status)
    }

    fn read_packets(
        &mut self,
        interface: NonNull<c_void>,
        packets: NonNull<VmnetPacketDescriptor>,
        packet_count: &mut c_int,
    ) -> VmnetStatus {
        // SAFETY: `interface` is an opaque handle returned by
        // `vmnet_start_interface`, `packets` points to a live descriptor array
        // whose initialized length is the input `packet_count`, and
        // `packet_count` is a valid mutable pointer for vmnet.framework to read
        // and update during the synchronous call.
        let status = unsafe {
            vmnet_sys::vmnet_read(
                interface.as_ptr(),
                packets.as_ptr(),
                ptr::from_mut(packet_count),
            )
        };

        VmnetStatus::from_raw(status)
    }

    fn write_packets(
        &mut self,
        interface: NonNull<c_void>,
        packets: NonNull<VmnetPacketDescriptor>,
        packet_count: &mut c_int,
    ) -> VmnetStatus {
        // SAFETY: `interface` is an opaque handle returned by
        // `vmnet_start_interface`, `packets` points to a live descriptor array
        // whose initialized length is the input `packet_count`, and
        // `packet_count` is a valid mutable pointer for vmnet.framework to read
        // and update during the synchronous call.
        let status = unsafe {
            vmnet_sys::vmnet_write(
                interface.as_ptr(),
                packets.as_ptr(),
                ptr::from_mut(packet_count),
            )
        };

        VmnetStatus::from_raw(status)
    }
}

pub struct SystemVmnetInterface {
    interface: NonNull<c_void>,
    packet_event_callback: Option<RcBlock<dyn Fn(u32, *mut c_void)>>,
}

impl fmt::Debug for SystemVmnetInterface {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SystemVmnetInterface(<owned>)")
    }
}

// SAFETY: `interface` is an opaque vmnet.framework handle. The optional
// `RcBlock` is a copied Objective-C heap block whose capture is `Send + Sync`;
// it is invoked only by the retained serial dispatch queue and may be released
// from any thread. Moving this owner does not invoke or dereference either
// value, and lifecycle operations still go through vmnet.framework with that
// explicit queue.
unsafe impl Send for SystemVmnetInterface {}

impl SystemVmnetInterface {
    const fn new(interface: NonNull<c_void>) -> Self {
        Self {
            interface,
            packet_event_callback: None,
        }
    }

    pub const fn as_raw_interface(&self) -> NonNull<c_void> {
        self.interface
    }
}

pub struct SystemVmnetInterfaceBackend {
    inner: SystemVmnetInterfaceBackendWithApi<SystemVmnetApi>,
}

impl fmt::Debug for SystemVmnetInterfaceBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SystemVmnetInterfaceBackend(<owned>)")
    }
}

impl Default for SystemVmnetInterfaceBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SystemVmnetInterfaceBackend {
    pub fn new() -> Self {
        Self {
            inner: SystemVmnetInterfaceBackendWithApi::with_api(SystemVmnetApi),
        }
    }
}

impl VmnetInterfaceBackend for SystemVmnetInterfaceBackend {
    type Interface = SystemVmnetInterface;

    fn start_interface(
        &mut self,
        descriptor: &VmnetInterfaceDescriptor,
    ) -> Result<VmnetStartedInterface<Self::Interface>, VmnetInterfaceStartError> {
        self.inner.start_interface(descriptor)
    }

    fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError> {
        self.inner.stop_interface(interface)
    }

    fn enable_packet_available_callback(
        &mut self,
        interface: &mut Self::Interface,
        callback: VmnetPacketAvailableCallback,
    ) -> Result<(), VmnetError> {
        self.inner
            .enable_packet_available_callback(interface, callback)
    }

    fn disable_and_drain_packet_available_callback(
        &mut self,
        interface: &mut Self::Interface,
    ) -> Result<(), VmnetError> {
        self.inner
            .disable_and_drain_packet_available_callback(interface)
    }
}

impl VmnetPacketIoBackend for SystemVmnetInterfaceBackend {
    type Interface = SystemVmnetInterface;

    fn read_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetReadPacket<'_>,
    ) -> Result<Option<usize>, VmnetPacketIoError> {
        self.inner.read_packet(interface, packet)
    }

    fn write_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetWritePacket<'_>,
    ) -> Result<(), VmnetPacketIoError> {
        self.inner.write_packet(interface, packet)
    }

    fn read_packet_batch(
        &mut self,
        interface: &mut Self::Interface,
        buffer: &mut [u8],
        packet_capacity: usize,
        requested_packets: usize,
        packet_lengths: &mut [usize],
    ) -> Result<usize, VmnetPacketIoError> {
        self.inner.read_packet_batch(
            interface,
            buffer,
            packet_capacity,
            requested_packets,
            packet_lengths,
        )
    }

    fn write_packet_batch(
        &mut self,
        interface: &mut Self::Interface,
        buffer: &[u8],
        packet_ranges: &[Range<usize>],
    ) -> Result<usize, VmnetPacketIoError> {
        self.inner
            .write_packet_batch(interface, buffer, packet_ranges)
    }
}

struct SystemVmnetInterfaceBackendWithApi<A> {
    api: A,
    queue: DispatchRetained<DispatchQueue>,
    completion_timeout: Duration,
    #[cfg(test)]
    hold_completion_channel_open: bool,
}

impl<A> SystemVmnetInterfaceBackendWithApi<A> {
    fn with_api(api: A) -> Self {
        Self {
            api,
            queue: DispatchQueue::new(
                "com.github.seven332.bangbang.vmnet",
                DispatchQueueAttr::SERIAL,
            ),
            completion_timeout: DEFAULT_VMNET_COMPLETION_TIMEOUT,
            #[cfg(test)]
            hold_completion_channel_open: false,
        }
    }

    #[cfg(test)]
    fn with_api_and_timeout(api: A, completion_timeout: Duration) -> Self {
        Self::with_api_timeout_and_channel_liveness(api, completion_timeout, true)
    }

    #[cfg(test)]
    fn with_api_timeout_and_channel_liveness(
        api: A,
        completion_timeout: Duration,
        hold_completion_channel_open: bool,
    ) -> Self {
        Self {
            api,
            queue: DispatchQueue::new(
                "com.github.seven332.bangbang.vmnet",
                DispatchQueueAttr::SERIAL,
            ),
            completion_timeout,
            hold_completion_channel_open,
        }
    }
}

impl<A> fmt::Debug for SystemVmnetInterfaceBackendWithApi<A> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SystemVmnetInterfaceBackendWithApi(<owned>)")
    }
}

struct VmnetStartCompletion {
    status: VmnetStatus,
    parameters: Result<RawVmnetInterfaceParameters, VmnetInterfaceParameterError>,
}

impl fmt::Debug for VmnetStartCompletion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetStartCompletion")
            .field("status", &self.status)
            .field("parameters", &self.parameters.as_ref().map(|_| "<decoded>"))
            .finish()
    }
}

impl<A> SystemVmnetInterfaceBackendWithApi<A>
where
    A: VmnetSystemApi,
{
    fn start_error_after_cleanup(
        &mut self,
        interface: &mut SystemVmnetInterface,
        source: VmnetError,
    ) -> VmnetInterfaceStartError {
        let disposition = match self.stop_interface(interface) {
            Ok(()) => VmnetInterfaceStartDisposition::Retryable,
            Err(_) => VmnetInterfaceStartDisposition::Terminal,
        };
        VmnetInterfaceStartError::start(source, disposition)
    }

    fn parameter_error_after_cleanup(
        &mut self,
        interface: &mut SystemVmnetInterface,
        source: VmnetInterfaceParameterError,
    ) -> VmnetInterfaceStartError {
        let disposition = match self.stop_interface(interface) {
            Ok(()) => VmnetInterfaceStartDisposition::Retryable,
            Err(_) => VmnetInterfaceStartDisposition::Terminal,
        };
        VmnetInterfaceStartError::parameters(source, disposition)
    }
}

impl<A> VmnetInterfaceBackend for SystemVmnetInterfaceBackendWithApi<A>
where
    A: VmnetSystemApi,
{
    type Interface = SystemVmnetInterface;

    fn start_interface(
        &mut self,
        descriptor: &VmnetInterfaceDescriptor,
    ) -> Result<VmnetStartedInterface<Self::Interface>, VmnetInterfaceStartError> {
        let (sender, receiver) = mpsc::channel();
        #[cfg(test)]
        let completion_channel_guard = self.hold_completion_channel_open.then(|| sender.clone());
        let completion = RcBlock::new(move |status: u32, interface_param: *mut c_void| {
            let status = VmnetStatus::from_raw(status);
            let parameters = if status == VmnetStatus::Success {
                decode_vmnet_interface_parameters(interface_param)
            } else {
                Err(VmnetInterfaceParameterError::new(
                    VmnetInterfaceParameterField::ResultDictionary,
                    VmnetInterfaceParameterProblem::Missing,
                ))
            };
            let _ = sender.send(VmnetStartCompletion { status, parameters });
        });
        let interface = self
            .api
            .start_interface(descriptor, &self.queue, &completion);
        // The public vmnet API copies an asynchronous completion block before
        // returning. Releasing our local block is what makes a missing external
        // callback owner observable as channel loss instead of a false timeout.
        drop(completion);
        let Some(interface) = interface else {
            let status = receiver
                .try_recv()
                .map(|completion| completion.status)
                .unwrap_or(VmnetStatus::Failure);
            let status = if status == VmnetStatus::Success {
                VmnetStatus::Failure
            } else {
                status
            };

            return Err(VmnetInterfaceStartError::start(
                VmnetError::new(VmnetOperation::StartInterface, status),
                VmnetInterfaceStartDisposition::Retryable,
            ));
        };
        let mut interface = SystemVmnetInterface::new(interface);
        let completion = match wait_for_vmnet_completion(
            receiver,
            VmnetOperation::StartInterface,
            self.completion_timeout,
        ) {
            Ok(completion) => completion,
            Err(source) => return Err(self.start_error_after_cleanup(&mut interface, source)),
        };
        #[cfg(test)]
        drop(completion_channel_guard);
        if completion.status != VmnetStatus::Success {
            let source = VmnetError::new(VmnetOperation::StartInterface, completion.status);
            return Err(self.start_error_after_cleanup(&mut interface, source));
        }
        let raw_parameters = match completion.parameters {
            Ok(parameters) => parameters,
            Err(source) => {
                return Err(self.parameter_error_after_cleanup(&mut interface, source));
            }
        };
        let parameters = match raw_parameters.validate(descriptor.result_policy) {
            Ok(parameters) => parameters,
            Err(source) => {
                return Err(self.parameter_error_after_cleanup(&mut interface, source));
            }
        };

        Ok(VmnetStartedInterface::new(interface, parameters))
    }

    fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError> {
        if interface.packet_event_callback.is_some() {
            self.disable_and_drain_packet_available_callback(interface)?;
        }
        let (sender, receiver) = mpsc::channel();
        #[cfg(test)]
        let completion_channel_guard = self.hold_completion_channel_open.then(|| sender.clone());
        let completion = RcBlock::new(move |status: u32| {
            let _ = sender.send(VmnetStatus::from_raw(status));
        });
        let schedule_status =
            self.api
                .stop_interface(interface.as_raw_interface(), &self.queue, &completion);
        // Match start ownership: only the framework's copied block may keep the
        // completion channel alive after the scheduling call returns.
        drop(completion);
        if schedule_status != VmnetStatus::Success {
            return Err(VmnetError::new(
                VmnetOperation::StopInterface,
                schedule_status,
            ));
        }

        let status = wait_for_vmnet_completion(
            receiver,
            VmnetOperation::StopInterface,
            self.completion_timeout,
        );
        #[cfg(test)]
        drop(completion_channel_guard);
        let status = status?;
        if status == VmnetStatus::Success {
            Ok(())
        } else {
            Err(VmnetError::new(VmnetOperation::StopInterface, status))
        }
    }

    fn enable_packet_available_callback(
        &mut self,
        interface: &mut Self::Interface,
        callback: VmnetPacketAvailableCallback,
    ) -> Result<(), VmnetError> {
        if interface.packet_event_callback.is_some() {
            return Err(VmnetError::new(
                VmnetOperation::EnablePacketEvents,
                VmnetStatus::InvalidArgument,
            ));
        }
        let event_callback = RcBlock::new(move |event_mask: u32, event: *mut c_void| {
            if event_mask & VMNET_INTERFACE_PACKETS_AVAILABLE_VALUE != 0 {
                callback.publish(decode_vmnet_packet_estimate(event));
            }
        });
        let status = self.api.set_event_callback(
            interface.as_raw_interface(),
            VMNET_INTERFACE_PACKETS_AVAILABLE_VALUE,
            Some(&self.queue),
            Some(&event_callback),
        );
        if status != VmnetStatus::Success {
            return Err(VmnetError::new(VmnetOperation::EnablePacketEvents, status));
        }
        interface.packet_event_callback = Some(event_callback);
        Ok(())
    }

    fn disable_and_drain_packet_available_callback(
        &mut self,
        interface: &mut Self::Interface,
    ) -> Result<(), VmnetError> {
        if interface.packet_event_callback.is_none() {
            return Ok(());
        }
        let status = self.api.set_event_callback(
            interface.as_raw_interface(),
            VMNET_INTERFACE_PACKETS_AVAILABLE_VALUE,
            None,
            None,
        );
        if status != VmnetStatus::Success {
            return Err(VmnetError::new(VmnetOperation::DisablePacketEvents, status));
        }

        let (sender, receiver) = mpsc::channel();
        self.queue.exec_async(move || {
            let _ = sender.send(());
        });
        wait_for_vmnet_completion(
            receiver,
            VmnetOperation::DrainPacketEvents,
            self.completion_timeout,
        )?;
        interface.packet_event_callback = None;
        Ok(())
    }
}

impl<A> VmnetPacketIoBackend for SystemVmnetInterfaceBackendWithApi<A>
where
    A: VmnetSystemApi,
{
    type Interface = SystemVmnetInterface;

    fn read_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetReadPacket<'_>,
    ) -> Result<Option<usize>, VmnetPacketIoError> {
        let mut packet_count = 1;
        let status = self.api.read_packets(
            interface.as_raw_interface(),
            NonNull::from(packet.as_mut_raw_descriptor()),
            &mut packet_count,
        );

        if status != VmnetStatus::Success {
            return Err(VmnetPacketIoError::vmnet(
                VmnetOperation::ReadPackets,
                status,
            ));
        }

        match packet_count {
            0 => Ok(None),
            1 => {
                let packet_size = packet.as_raw_descriptor().packet_size();
                let buffer_len = packet.iov().iov_len;
                if packet_size > buffer_len {
                    return Err(VmnetPacketIoError::ReadPacketSizeExceedsBuffer {
                        packet_size,
                        buffer_len,
                    });
                }

                Ok(Some(packet_size))
            }
            actual => Err(VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::ReadPackets,
                expected: VmnetPacketCountExpectation::ZeroOrOne,
                actual,
            }),
        }
    }

    fn write_packet(
        &mut self,
        interface: &mut Self::Interface,
        packet: &mut VmnetWritePacket<'_>,
    ) -> Result<(), VmnetPacketIoError> {
        let mut packet_count = 1;
        let status = self.api.write_packets(
            interface.as_raw_interface(),
            NonNull::from(packet.as_mut_raw_descriptor()),
            &mut packet_count,
        );

        if status != VmnetStatus::Success {
            return Err(VmnetPacketIoError::vmnet(
                VmnetOperation::WritePackets,
                status,
            ));
        }

        if packet_count == 1 {
            Ok(())
        } else {
            Err(VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::WritePackets,
                expected: VmnetPacketCountExpectation::One,
                actual: packet_count,
            })
        }
    }

    fn read_packet_batch(
        &mut self,
        interface: &mut Self::Interface,
        buffer: &mut [u8],
        packet_capacity: usize,
        requested_packets: usize,
        packet_lengths: &mut [usize],
    ) -> Result<usize, VmnetPacketIoError> {
        validate_read_batch_layout(
            buffer.len(),
            packet_capacity,
            requested_packets,
            packet_lengths.len(),
        )?;

        let mut iovs: [libc::iovec; VMNET_MAX_PACKETS_PER_OPERATION] =
            std::array::from_fn(|_| libc::iovec {
                iov_base: ptr::null_mut(),
                iov_len: 0,
            });
        let mut descriptors: [VmnetPacketDescriptor; VMNET_MAX_PACKETS_PER_OPERATION] =
            std::array::from_fn(|_| VmnetPacketDescriptor {
                vm_pkt_size: 0,
                vm_pkt_iov: ptr::null_mut(),
                vm_pkt_iovcnt: 0,
                vm_flags: 0,
            });
        for (packet_index, (iov, descriptor)) in iovs
            .iter_mut()
            .zip(descriptors.iter_mut())
            .take(requested_packets)
            .enumerate()
        {
            let start = packet_index * packet_capacity;
            let packet_buffer = buffer.get_mut(start..start + packet_capacity).ok_or(
                VmnetPacketIoError::InvalidBatch {
                    message: "read packet range exceeds the aggregate buffer",
                },
            )?;
            *iov = libc::iovec {
                iov_base: packet_buffer.as_mut_ptr().cast::<c_void>(),
                iov_len: packet_buffer.len(),
            };
            *descriptor = VmnetPacketDescriptor {
                vm_pkt_size: packet_buffer.len(),
                vm_pkt_iov: ptr::from_mut(iov),
                vm_pkt_iovcnt: 1,
                vm_flags: 0,
            };
        }

        let mut packet_count =
            c_int::try_from(requested_packets).map_err(|_| VmnetPacketIoError::InvalidBatch {
                message: "read packet count does not fit the vmnet ABI",
            })?;
        let descriptor_head = descriptors
            .first_mut()
            .ok_or(VmnetPacketIoError::InvalidBatch {
                message: "read packet descriptor batch is unexpectedly empty",
            })?;
        let status = self.api.read_packets(
            interface.as_raw_interface(),
            NonNull::from(descriptor_head),
            &mut packet_count,
        );
        if status != VmnetStatus::Success {
            return Err(VmnetPacketIoError::vmnet(
                VmnetOperation::ReadPackets,
                status,
            ));
        }
        let completed = usize::try_from(packet_count)
            .ok()
            .filter(|completed| *completed <= requested_packets);
        let Some(completed) = completed else {
            return Err(VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::ReadPackets,
                expected: VmnetPacketCountExpectation::AtMost(requested_packets),
                actual: packet_count,
            });
        };
        for (descriptor, packet_len_slot) in descriptors
            .iter()
            .zip(packet_lengths.iter_mut())
            .take(completed)
        {
            let packet_size = descriptor.packet_size();
            if packet_size > packet_capacity {
                return Err(VmnetPacketIoError::ReadPacketSizeExceedsBuffer {
                    packet_size,
                    buffer_len: packet_capacity,
                });
            }
            *packet_len_slot = packet_size;
        }
        Ok(completed)
    }

    fn write_packet_batch(
        &mut self,
        interface: &mut Self::Interface,
        buffer: &[u8],
        packet_ranges: &[Range<usize>],
    ) -> Result<usize, VmnetPacketIoError> {
        validate_write_batch_layout(buffer, packet_ranges)?;

        let mut iovs: [libc::iovec; VMNET_MAX_PACKETS_PER_OPERATION] =
            std::array::from_fn(|_| libc::iovec {
                iov_base: ptr::null_mut(),
                iov_len: 0,
            });
        let mut descriptors: [VmnetPacketDescriptor; VMNET_MAX_PACKETS_PER_OPERATION] =
            std::array::from_fn(|_| VmnetPacketDescriptor {
                vm_pkt_size: 0,
                vm_pkt_iov: ptr::null_mut(),
                vm_pkt_iovcnt: 0,
                vm_flags: 0,
            });
        for ((iov, descriptor), range) in iovs
            .iter_mut()
            .zip(descriptors.iter_mut())
            .zip(packet_ranges)
        {
            let packet = buffer
                .get(range.clone())
                .ok_or(VmnetPacketIoError::InvalidBatch {
                    message: "write packet range exceeds the aggregate buffer",
                })?;
            *iov = libc::iovec {
                iov_base: packet.as_ptr().cast::<c_void>().cast_mut(),
                iov_len: packet.len(),
            };
            *descriptor = VmnetPacketDescriptor {
                vm_pkt_size: packet.len(),
                vm_pkt_iov: ptr::from_mut(iov),
                vm_pkt_iovcnt: 1,
                vm_flags: 0,
            };
        }

        let requested_packets = packet_ranges.len();
        let mut packet_count =
            c_int::try_from(requested_packets).map_err(|_| VmnetPacketIoError::InvalidBatch {
                message: "write packet count does not fit the vmnet ABI",
            })?;
        let descriptor_head = descriptors
            .first_mut()
            .ok_or(VmnetPacketIoError::InvalidBatch {
                message: "write packet descriptor batch is unexpectedly empty",
            })?;
        let status = self.api.write_packets(
            interface.as_raw_interface(),
            NonNull::from(descriptor_head),
            &mut packet_count,
        );
        if status != VmnetStatus::Success {
            return Err(VmnetPacketIoError::vmnet(
                VmnetOperation::WritePackets,
                status,
            ));
        }
        usize::try_from(packet_count)
            .ok()
            .filter(|completed| *completed <= requested_packets)
            .ok_or(VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::WritePackets,
                expected: VmnetPacketCountExpectation::AtMost(requested_packets),
                actual: packet_count,
            })
    }
}

fn wait_for_vmnet_completion<T>(
    receiver: mpsc::Receiver<T>,
    operation: VmnetOperation,
    timeout: Duration,
) -> Result<T, VmnetError> {
    receiver.recv_timeout(timeout).map_err(|error| {
        VmnetError::completion(
            operation,
            match error {
                mpsc::RecvTimeoutError::Timeout => VmnetCompletionError::TimedOut,
                mpsc::RecvTimeoutError::Disconnected => VmnetCompletionError::ChannelClosed,
            },
        )
    })
}

pub struct StartedVmnetInterface<B>
where
    B: VmnetInterfaceBackend,
{
    backend: B,
    interface: Option<B::Interface>,
    parameters: VmnetInterfaceParameters,
    uncertain: bool,
}

impl<B> StartedVmnetInterface<B>
where
    B: VmnetInterfaceBackend,
{
    pub fn start(
        mut backend: B,
        config: &VmnetInterfaceConfig,
    ) -> Result<Self, VmnetInterfaceStartError> {
        let descriptor = backend
            .build_interface_descriptor(config)
            .map_err(|source| VmnetInterfaceStartError::Descriptor { source })?;
        let started = backend.start_interface(&descriptor)?;
        let (interface, parameters) = started.into_parts();

        Ok(Self {
            backend,
            interface: Some(interface),
            parameters,
            uncertain: false,
        })
    }

    pub const fn is_started(&self) -> bool {
        self.interface.is_some() && !self.uncertain
    }

    pub const fn is_uncertain(&self) -> bool {
        self.uncertain
    }

    pub const fn parameters(&self) -> &VmnetInterfaceParameters {
        &self.parameters
    }

    pub fn stop(&mut self) -> Result<(), VmnetError> {
        if self.uncertain {
            return Err(VmnetError::new(
                VmnetOperation::StopInterface,
                VmnetStatus::Failure,
            ));
        }
        if let Some(interface) = self.interface.as_mut() {
            match self.backend.stop_interface(interface) {
                Ok(()) => self.interface = None,
                Err(source) => {
                    self.uncertain = true;
                    return Err(source);
                }
            }
        }

        Ok(())
    }
}

impl<B> fmt::Debug for StartedVmnetInterface<B>
where
    B: VmnetInterfaceBackend,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StartedVmnetInterface")
            .field("backend", &"<owned>")
            .field("interface", &self.interface.as_ref().map(|_| "<owned>"))
            .field("parameters", &self.parameters)
            .field("uncertain", &self.uncertain)
            .finish()
    }
}

impl<B> Drop for StartedVmnetInterface<B>
where
    B: VmnetInterfaceBackend,
{
    fn drop(&mut self) {
        if !self.uncertain {
            match self.stop() {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StartedVmnetPacketIoInterface;

#[derive(Debug)]
pub struct StartedVmnetPacketIoBackend<B>
where
    B: VmnetInterfaceBackend
        + VmnetPacketIoBackend<Interface = <B as VmnetInterfaceBackend>::Interface>,
{
    started: StartedVmnetInterface<B>,
    packet_events_enabled: bool,
}

impl<B> StartedVmnetPacketIoBackend<B>
where
    B: VmnetInterfaceBackend
        + VmnetPacketIoBackend<Interface = <B as VmnetInterfaceBackend>::Interface>,
{
    pub fn start(
        backend: B,
        config: &VmnetInterfaceConfig,
    ) -> Result<(Self, StartedVmnetPacketIoInterface), VmnetInterfaceStartError> {
        Ok((
            Self {
                started: StartedVmnetInterface::start(backend, config)?,
                packet_events_enabled: false,
            },
            StartedVmnetPacketIoInterface,
        ))
    }

    pub const fn is_started(&self) -> bool {
        self.started.is_started()
    }

    pub const fn parameters(&self) -> &VmnetInterfaceParameters {
        self.started.parameters()
    }

    pub fn enable_packet_available_callback(
        &mut self,
        callback: VmnetPacketAvailableCallback,
    ) -> Result<(), VmnetError> {
        if self.packet_events_enabled || !self.started.is_started() {
            return Err(VmnetError::new(
                VmnetOperation::EnablePacketEvents,
                VmnetStatus::InvalidArgument,
            ));
        }
        let Some(interface) = self.started.interface.as_mut() else {
            return Err(VmnetError::new(
                VmnetOperation::EnablePacketEvents,
                VmnetStatus::Failure,
            ));
        };
        self.started
            .backend
            .enable_packet_available_callback(interface, callback)?;
        self.packet_events_enabled = true;
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), VmnetError> {
        if self.started.is_uncertain() {
            return Err(VmnetError::new(
                if self.packet_events_enabled {
                    VmnetOperation::DisablePacketEvents
                } else {
                    VmnetOperation::StopInterface
                },
                VmnetStatus::Failure,
            ));
        }
        if self.packet_events_enabled {
            let Some(interface) = self.started.interface.as_mut() else {
                self.started.uncertain = true;
                return Err(VmnetError::new(
                    VmnetOperation::DisablePacketEvents,
                    VmnetStatus::Failure,
                ));
            };
            if let Err(source) = self
                .started
                .backend
                .disable_and_drain_packet_available_callback(interface)
            {
                self.started.uncertain = true;
                return Err(source);
            }
            self.packet_events_enabled = false;
        }
        self.started.stop()
    }
}

impl<B> Drop for StartedVmnetPacketIoBackend<B>
where
    B: VmnetInterfaceBackend
        + VmnetPacketIoBackend<Interface = <B as VmnetInterfaceBackend>::Interface>,
{
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl<B> VmnetPacketIoBackend for StartedVmnetPacketIoBackend<B>
where
    B: VmnetInterfaceBackend
        + VmnetPacketIoBackend<Interface = <B as VmnetInterfaceBackend>::Interface>,
{
    type Interface = StartedVmnetPacketIoInterface;

    fn read_packet(
        &mut self,
        _interface: &mut Self::Interface,
        packet: &mut VmnetReadPacket<'_>,
    ) -> Result<Option<usize>, VmnetPacketIoError> {
        if !self.started.is_started() {
            return Err(VmnetPacketIoError::InterfaceStopped);
        }
        let Some(interface) = self.started.interface.as_mut() else {
            return Err(VmnetPacketIoError::InterfaceStopped);
        };

        self.started.backend.read_packet(interface, packet)
    }

    fn write_packet(
        &mut self,
        _interface: &mut Self::Interface,
        packet: &mut VmnetWritePacket<'_>,
    ) -> Result<(), VmnetPacketIoError> {
        if !self.started.is_started() {
            return Err(VmnetPacketIoError::InterfaceStopped);
        }
        let Some(interface) = self.started.interface.as_mut() else {
            return Err(VmnetPacketIoError::InterfaceStopped);
        };

        self.started.backend.write_packet(interface, packet)
    }

    fn read_packet_batch(
        &mut self,
        _interface: &mut Self::Interface,
        buffer: &mut [u8],
        packet_capacity: usize,
        requested_packets: usize,
        packet_lengths: &mut [usize],
    ) -> Result<usize, VmnetPacketIoError> {
        if !self.started.is_started() {
            return Err(VmnetPacketIoError::InterfaceStopped);
        }
        let Some(interface) = self.started.interface.as_mut() else {
            return Err(VmnetPacketIoError::InterfaceStopped);
        };
        self.started.backend.read_packet_batch(
            interface,
            buffer,
            packet_capacity,
            requested_packets,
            packet_lengths,
        )
    }

    fn write_packet_batch(
        &mut self,
        _interface: &mut Self::Interface,
        buffer: &[u8],
        packet_ranges: &[Range<usize>],
    ) -> Result<usize, VmnetPacketIoError> {
        if !self.started.is_started() {
            return Err(VmnetPacketIoError::InterfaceStopped);
        }
        let Some(interface) = self.started.interface.as_mut() else {
            return Err(VmnetPacketIoError::InterfaceStopped);
        };
        self.started
            .backend
            .write_packet_batch(interface, buffer, packet_ranges)
    }
}

pub trait VmnetInterfaceLifecycle: fmt::Debug + Send + 'static {
    type Interface: fmt::Debug + Send + 'static;

    fn start_interface(
        &mut self,
        config: &VmnetInterfaceConfig,
    ) -> Result<Self::Interface, VmnetError>;

    fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError>;
}

pub struct OwnedVmnetInterface<L>
where
    L: VmnetInterfaceLifecycle,
{
    lifecycle: L,
    interface: Option<L::Interface>,
    uncertain: bool,
}

impl<L> OwnedVmnetInterface<L>
where
    L: VmnetInterfaceLifecycle,
{
    pub fn start(mut lifecycle: L, config: &VmnetInterfaceConfig) -> Result<Self, VmnetError> {
        let interface = lifecycle.start_interface(config)?;

        Ok(Self {
            lifecycle,
            interface: Some(interface),
            uncertain: false,
        })
    }

    pub const fn is_started(&self) -> bool {
        self.interface.is_some() && !self.uncertain
    }

    pub const fn is_uncertain(&self) -> bool {
        self.uncertain
    }

    pub fn stop(&mut self) -> Result<(), VmnetError> {
        if self.uncertain {
            return Err(VmnetError::new(
                VmnetOperation::StopInterface,
                VmnetStatus::Failure,
            ));
        }
        if let Some(interface) = self.interface.as_mut() {
            match self.lifecycle.stop_interface(interface) {
                Ok(()) => self.interface = None,
                Err(source) => {
                    self.uncertain = true;
                    return Err(source);
                }
            }
        }

        Ok(())
    }
}

impl<L> fmt::Debug for OwnedVmnetInterface<L>
where
    L: VmnetInterfaceLifecycle,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OwnedVmnetInterface")
            .field("lifecycle", &"<owned>")
            .field("interface", &self.interface.as_ref().map(|_| "<owned>"))
            .field("uncertain", &self.uncertain)
            .finish()
    }
}

impl<L> Drop for OwnedVmnetInterface<L>
where
    L: VmnetInterfaceLifecycle,
{
    fn drop(&mut self) {
        if !self.uncertain {
            match self.stop() {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::error::Error;
    use std::ffi::{CStr, c_int, c_void};
    use std::mem::{align_of, offset_of, size_of};
    use std::ptr::{self, NonNull};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use bangbang_runtime::network::{
        GuestMacAddress, VIRTIO_NET_MAX_BUFFER_SIZE, VIRTIO_NET_MAX_MTU, VIRTIO_NET_MIN_MTU,
        VIRTIO_NET_QUEUE_SIZE, VIRTIO_NET_TX_HEADER_SIZE, VirtioNetworkRxPacketSource,
    };
    use block2::Block;
    use dispatch2::DispatchQueue;

    use crate::host_network::virtio_vmnet::VmnetVirtioNetworkPacketIo;

    use super::{
        OwnedVmnetInterface, StartedVmnetInterface, StartedVmnetPacketIoBackend,
        VMNET_BRIDGED_MODE_VALUE, VMNET_HOST_MODE_VALUE, VMNET_SHARED_MODE_VALUE, VmnetError,
        VmnetHostDeviceNameConfigError, VmnetInterfaceBackend, VmnetInterfaceConfig,
        VmnetInterfaceConfigError, VmnetInterfaceDescriptor, VmnetInterfaceDescriptorError,
        VmnetInterfaceLifecycle, VmnetInterfaceParameterError, VmnetInterfaceParameterField,
        VmnetInterfaceParameterProblem, VmnetInterfaceParameters, VmnetInterfaceStartDisposition,
        VmnetInterfaceStartError, VmnetMode, VmnetOperation, VmnetPacketAvailableCallback,
        VmnetPacketCountExpectation, VmnetPacketDescriptor, VmnetPacketDescriptorError,
        VmnetPacketIoBackend, VmnetPacketIoError, VmnetReadPacket, VmnetStartedInterface,
        VmnetStatus, VmnetSystemApi, VmnetWritePacket,
    };

    #[derive(Debug, Clone, PartialEq, Eq)]
    struct RecordedVmnetInterface {
        id: u64,
    }

    #[derive(Debug, Clone)]
    struct RecordingVmnetLifecycle {
        events: Arc<Mutex<Vec<String>>>,
        start_status: Option<VmnetStatus>,
        stop_status: Option<VmnetStatus>,
    }

    impl RecordingVmnetLifecycle {
        fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                start_status: None,
                stop_status: None,
            }
        }

        fn with_start_status(mut self, status: VmnetStatus) -> Self {
            self.start_status = Some(status);
            self
        }

        fn with_stop_status(mut self, status: VmnetStatus) -> Self {
            self.stop_status = Some(status);
            self
        }

        fn events(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.events)
        }
    }

    impl VmnetInterfaceLifecycle for RecordingVmnetLifecycle {
        type Interface = RecordedVmnetInterface;

        fn start_interface(
            &mut self,
            config: &VmnetInterfaceConfig,
        ) -> Result<Self::Interface, VmnetError> {
            push_event(&self.events, format!("start:{}", config.mode()));
            if let Some(status) = self.start_status {
                return Err(VmnetError::new(VmnetOperation::StartInterface, status));
            }

            Ok(RecordedVmnetInterface { id: 7 })
        }

        fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError> {
            push_event(&self.events, format!("stop:{}", interface.id));
            if let Some(status) = self.stop_status {
                return Err(VmnetError::new(VmnetOperation::StopInterface, status));
            }

            Ok(())
        }
    }

    #[derive(Debug, Clone)]
    struct RecordingVmnetBackend {
        events: Arc<Mutex<Vec<String>>>,
        descriptor_error: Option<VmnetInterfaceDescriptorError>,
        start_status: Option<VmnetStatus>,
        stop_statuses: VecDeque<VmnetStatus>,
        read_result: Result<Option<usize>, VmnetPacketIoError>,
        write_result: Result<(), VmnetPacketIoError>,
    }

    impl RecordingVmnetBackend {
        fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                descriptor_error: None,
                start_status: None,
                stop_statuses: VecDeque::new(),
                read_result: Ok(None),
                write_result: Ok(()),
            }
        }

        fn with_descriptor_error(mut self, error: VmnetInterfaceDescriptorError) -> Self {
            self.descriptor_error = Some(error);
            self
        }

        fn with_start_status(mut self, status: VmnetStatus) -> Self {
            self.start_status = Some(status);
            self
        }

        fn with_stop_status(mut self, status: VmnetStatus) -> Self {
            self.stop_statuses.push_back(status);
            self
        }

        fn with_read_result(mut self, result: Result<Option<usize>, VmnetPacketIoError>) -> Self {
            self.read_result = result;
            self
        }

        fn with_write_result(mut self, result: Result<(), VmnetPacketIoError>) -> Self {
            self.write_result = result;
            self
        }

        fn events(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.events)
        }
    }

    impl VmnetInterfaceBackend for RecordingVmnetBackend {
        type Interface = RecordedVmnetInterface;

        fn build_interface_descriptor(
            &mut self,
            config: &VmnetInterfaceConfig,
        ) -> Result<VmnetInterfaceDescriptor, VmnetInterfaceDescriptorError> {
            push_event(&self.events, format!("descriptor:{}", config.mode()));
            if let Some(error) = self.descriptor_error {
                return Err(error);
            }

            VmnetInterfaceDescriptor::new(config)
        }

        fn start_interface(
            &mut self,
            descriptor: &VmnetInterfaceDescriptor,
        ) -> Result<VmnetStartedInterface<Self::Interface>, VmnetInterfaceStartError> {
            push_event(
                &self.events,
                format!("start:{}", descriptor_mode(descriptor)),
            );
            if let Some(status) = self.start_status {
                return Err(VmnetInterfaceStartError::Start {
                    source: VmnetError::new(VmnetOperation::StartInterface, status),
                    disposition: VmnetInterfaceStartDisposition::Retryable,
                });
            }

            Ok(VmnetStartedInterface::new(
                RecordedVmnetInterface { id: 9 },
                VmnetInterfaceParameters::for_test(
                    GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 9]),
                    1500,
                    2048,
                ),
            ))
        }

        fn stop_interface(&mut self, interface: &mut Self::Interface) -> Result<(), VmnetError> {
            push_event(&self.events, format!("stop:{}", interface.id));
            if let Some(status) = self.stop_statuses.pop_front() {
                return Err(VmnetError::new(VmnetOperation::StopInterface, status));
            }

            Ok(())
        }

        fn enable_packet_available_callback(
            &mut self,
            _interface: &mut Self::Interface,
            _callback: VmnetPacketAvailableCallback,
        ) -> Result<(), VmnetError> {
            Ok(())
        }

        fn disable_and_drain_packet_available_callback(
            &mut self,
            _interface: &mut Self::Interface,
        ) -> Result<(), VmnetError> {
            Ok(())
        }
    }

    impl VmnetPacketIoBackend for RecordingVmnetBackend {
        type Interface = RecordedVmnetInterface;

        fn read_packet(
            &mut self,
            interface: &mut Self::Interface,
            _packet: &mut VmnetReadPacket<'_>,
        ) -> Result<Option<usize>, VmnetPacketIoError> {
            push_event(&self.events, format!("read:{}", interface.id));
            self.read_result.clone()
        }

        fn write_packet(
            &mut self,
            interface: &mut Self::Interface,
            packet: &mut VmnetWritePacket<'_>,
        ) -> Result<(), VmnetPacketIoError> {
            push_event(
                &self.events,
                format!(
                    "write:{}:{}",
                    interface.id,
                    packet.as_raw_descriptor().packet_size()
                ),
            );
            self.write_result.clone()
        }
    }

    #[derive(Debug, Clone)]
    struct RecordingVmnetSystemApi {
        events: Arc<Mutex<Vec<String>>>,
        start_handle: Option<usize>,
        start_completion: VmnetStatus,
        deliver_start_completion: bool,
        omit_start_mtu: bool,
        stop_schedule_statuses: VecDeque<VmnetStatus>,
        stop_completion_statuses: VecDeque<VmnetStatus>,
        deliver_stop_completion: bool,
        read_status: VmnetStatus,
        read_packet_count: c_int,
        read_packet_size: usize,
        write_status: VmnetStatus,
        write_packet_count: c_int,
        event_callback_statuses: VecDeque<VmnetStatus>,
        packet_event_estimate: Option<Option<u64>>,
    }

    impl RecordingVmnetSystemApi {
        fn new() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                start_handle: Some(0x10),
                start_completion: VmnetStatus::Success,
                deliver_start_completion: true,
                omit_start_mtu: false,
                stop_schedule_statuses: VecDeque::new(),
                stop_completion_statuses: VecDeque::new(),
                deliver_stop_completion: true,
                read_status: VmnetStatus::Success,
                read_packet_count: 1,
                read_packet_size: 64,
                write_status: VmnetStatus::Success,
                write_packet_count: 1,
                event_callback_statuses: VecDeque::new(),
                packet_event_estimate: None,
            }
        }

        fn with_null_start_handle(mut self) -> Self {
            self.start_handle = None;
            self
        }

        fn with_start_completion(mut self, status: VmnetStatus) -> Self {
            self.start_completion = status;
            self
        }

        fn without_start_completion(mut self) -> Self {
            self.deliver_start_completion = false;
            self
        }

        fn with_missing_start_mtu(mut self) -> Self {
            self.omit_start_mtu = true;
            self
        }

        fn with_stop_schedule_status(mut self, status: VmnetStatus) -> Self {
            self.stop_schedule_statuses.push_back(status);
            self
        }

        fn with_stop_completion_status(mut self, status: VmnetStatus) -> Self {
            self.stop_completion_statuses.push_back(status);
            self
        }

        fn without_stop_completion(mut self) -> Self {
            self.deliver_stop_completion = false;
            self
        }

        fn with_read_result(
            mut self,
            status: VmnetStatus,
            packet_count: c_int,
            packet_size: usize,
        ) -> Self {
            self.read_status = status;
            self.read_packet_count = packet_count;
            self.read_packet_size = packet_size;
            self
        }

        fn with_write_result(mut self, status: VmnetStatus, packet_count: c_int) -> Self {
            self.write_status = status;
            self.write_packet_count = packet_count;
            self
        }

        fn with_event_callback_status(mut self, status: VmnetStatus) -> Self {
            self.event_callback_statuses.push_back(status);
            self
        }

        fn with_packet_event(mut self, estimate: Option<u64>) -> Self {
            self.packet_event_estimate = Some(estimate);
            self
        }

        fn events(&self) -> Arc<Mutex<Vec<String>>> {
            Arc::clone(&self.events)
        }
    }

    impl VmnetSystemApi for RecordingVmnetSystemApi {
        fn start_interface(
            &mut self,
            descriptor: &VmnetInterfaceDescriptor,
            _queue: &DispatchQueue,
            completion: &Block<dyn Fn(u32, *mut c_void)>,
        ) -> Option<NonNull<c_void>> {
            push_event(
                &self.events,
                format!("system-start:{}", descriptor_mode(descriptor)),
            );
            if self.deliver_start_completion {
                let parameters = (self.start_completion == VmnetStatus::Success)
                    .then(|| successful_result_dictionary(descriptor, !self.omit_start_mtu));
                completion.call((
                    self.start_completion.raw_value(),
                    parameters
                        .as_ref()
                        .map_or(ptr::null_mut(), super::OwnedXpcObject::as_ptr),
                ));
            }

            self.start_handle.map(fake_interface)
        }

        fn stop_interface(
            &mut self,
            interface: NonNull<c_void>,
            _queue: &DispatchQueue,
            completion: &Block<dyn Fn(u32)>,
        ) -> VmnetStatus {
            push_event(
                &self.events,
                format!("system-stop:{:x}", interface.as_ptr() as usize),
            );
            let schedule_status = self
                .stop_schedule_statuses
                .pop_front()
                .unwrap_or(VmnetStatus::Success);
            if schedule_status == VmnetStatus::Success && self.deliver_stop_completion {
                let completion_status = self
                    .stop_completion_statuses
                    .pop_front()
                    .unwrap_or(VmnetStatus::Success);
                completion.call((completion_status.raw_value(),));
            }

            schedule_status
        }

        fn set_event_callback(
            &mut self,
            interface: NonNull<c_void>,
            event_mask: u32,
            queue: Option<&DispatchQueue>,
            callback: Option<&Block<dyn Fn(u32, *mut c_void)>>,
        ) -> VmnetStatus {
            let enabling = queue.is_some() && callback.is_some();
            push_event(
                &self.events,
                format!(
                    "system-events:{:x}:{}:{event_mask}",
                    interface.as_ptr() as usize,
                    if enabling { "enable" } else { "disable" }
                ),
            );
            let status = self
                .event_callback_statuses
                .pop_front()
                .unwrap_or(VmnetStatus::Success);
            if status == VmnetStatus::Success
                && let (Some(callback), Some(estimate)) =
                    (callback, self.packet_event_estimate.take())
            {
                let event = super::OwnedXpcObject::dictionary()
                    .expect("test vmnet event dictionary should allocate");
                if let Some(estimate) = estimate {
                    // SAFETY: The test dictionary and framework key are live,
                    // and primitive insertion retains no Rust borrow.
                    unsafe {
                        super::xpc::xpc_dictionary_set_uint64(
                            event.as_ptr(),
                            super::vmnet_estimated_packets_available_key()
                                .expect("test estimate key should exist"),
                            estimate,
                        );
                    }
                }
                callback.call((
                    super::VMNET_INTERFACE_PACKETS_AVAILABLE_VALUE,
                    event.as_ptr(),
                ));
            }
            status
        }

        fn read_packets(
            &mut self,
            interface: NonNull<c_void>,
            packets: NonNull<VmnetPacketDescriptor>,
            packet_count: &mut c_int,
        ) -> VmnetStatus {
            let requested_packet_count = *packet_count;
            push_event(
                &self.events,
                format!(
                    "system-read:{:x}:{}",
                    interface.as_ptr() as usize,
                    *packet_count
                ),
            );
            *packet_count = self.read_packet_count;
            if self.read_status == VmnetStatus::Success
                && self.read_packet_count > 0
                && self.read_packet_count <= requested_packet_count
            {
                for packet_index in 0..self.read_packet_count as usize {
                    // SAFETY: The fake backend is called with a contiguous
                    // descriptor prefix of `requested_packet_count` elements.
                    unsafe {
                        (*packets.as_ptr().add(packet_index)).vm_pkt_size = self.read_packet_size;
                    }
                }
            }

            self.read_status
        }

        fn write_packets(
            &mut self,
            interface: NonNull<c_void>,
            _packets: NonNull<VmnetPacketDescriptor>,
            packet_count: &mut c_int,
        ) -> VmnetStatus {
            push_event(
                &self.events,
                format!(
                    "system-write:{:x}:{}",
                    interface.as_ptr() as usize,
                    *packet_count
                ),
            );
            *packet_count = self.write_packet_count;

            self.write_status
        }
    }

    fn successful_result_dictionary(
        descriptor: &VmnetInterfaceDescriptor,
        include_mtu: bool,
    ) -> super::OwnedXpcObject {
        let mac = descriptor
            .result_policy
            .requested_mac
            .unwrap_or_else(|| GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 0x10]));
        let effective_mtu = u64::from(descriptor.result_policy.requested_mtu.unwrap_or(1500));
        let maximum_packet_size = effective_mtu.max(2048);
        let mac = mac.to_string();
        test_result_dictionary(
            Some(&mac),
            include_mtu.then_some(effective_mtu),
            Some(maximum_packet_size),
        )
    }

    fn test_result_dictionary(
        mac: Option<&str>,
        effective_mtu: Option<u64>,
        maximum_packet_size: Option<u64>,
    ) -> super::OwnedXpcObject {
        let dictionary = super::OwnedXpcObject::dictionary()
            .expect("test vmnet result dictionary should allocate");
        if let Some(mac) = mac {
            set_dictionary_string(
                &dictionary,
                super::vmnet_mac_address_key().expect("test MAC key should exist"),
                mac,
            );
        }
        if let Some(effective_mtu) = effective_mtu {
            set_dictionary_uint64(
                &dictionary,
                super::vmnet_mtu_key().expect("test MTU key should exist"),
                effective_mtu,
            );
        }
        if let Some(maximum_packet_size) = maximum_packet_size {
            set_dictionary_uint64(
                &dictionary,
                super::vmnet_max_packet_size_key().expect("test maximum-packet key should exist"),
                maximum_packet_size,
            );
        }
        dictionary
    }

    fn set_dictionary_string(
        dictionary: &super::OwnedXpcObject,
        key: *const std::ffi::c_char,
        value: &str,
    ) {
        let value = std::ffi::CString::new(value).expect("test XPC string should contain no NUL");
        // SAFETY: The dictionary and key are live, and `value` is a valid C
        // string for the duration of this insertion.
        unsafe {
            super::xpc::xpc_dictionary_set_string(dictionary.as_ptr(), key, value.as_ptr());
        }
    }

    fn set_dictionary_uint64(
        dictionary: &super::OwnedXpcObject,
        key: *const std::ffi::c_char,
        value: u64,
    ) {
        // SAFETY: The dictionary and key are live and primitive insertion does
        // not retain borrowed Rust data.
        unsafe {
            super::xpc::xpc_dictionary_set_uint64(dictionary.as_ptr(), key, value);
        }
    }

    fn set_dictionary_uuid(
        dictionary: &super::OwnedXpcObject,
        key: *const std::ffi::c_char,
        value: [u8; super::VMNET_INTERFACE_ID_LEN],
    ) {
        // SAFETY: The dictionary and key are live. XPC copies the fixed-size
        // UUID bytes before this call returns.
        unsafe {
            super::xpc::xpc_dictionary_set_uuid(dictionary.as_ptr(), key, value.as_ptr());
        }
    }

    fn decode_test_parameters(
        dictionary: &super::OwnedXpcObject,
        config: &VmnetInterfaceConfig,
    ) -> Result<VmnetInterfaceParameters, VmnetInterfaceParameterError> {
        let descriptor =
            VmnetInterfaceDescriptor::new(config).expect("test vmnet descriptor should be created");
        super::decode_vmnet_interface_parameters(dictionary.as_ptr())?
            .validate(descriptor.result_policy)
    }

    fn raw_test_parameters(
        returned_mac: Option<GuestMacAddress>,
        effective_mtu: u64,
        maximum_packet_size: u64,
    ) -> super::RawVmnetInterfaceParameters {
        super::RawVmnetInterfaceParameters {
            returned_mac,
            effective_mtu,
            maximum_packet_size,
            interface_id: None,
            read_max_packets: None,
            write_max_packets: None,
        }
    }

    fn assert_parameter_error<T: std::fmt::Debug>(
        result: Result<T, VmnetInterfaceParameterError>,
        field: VmnetInterfaceParameterField,
        problem: VmnetInterfaceParameterProblem,
    ) {
        let error = result.expect_err("vmnet parameter fixture should fail");
        assert_eq!(error.field(), field);
        assert_eq!(error.problem(), problem);
    }

    fn push_event(events: &Arc<Mutex<Vec<String>>>, event: String) {
        match events.lock() {
            Ok(mut guard) => guard.push(event),
            Err(poisoned) => poisoned.into_inner().push(event),
        }
    }

    fn recorded_events(events: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        match events.lock() {
            Ok(guard) => guard.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    fn fake_interface(handle: usize) -> NonNull<c_void> {
        NonNull::new(handle as *mut c_void).expect("fake vmnet interface handle must be non-null")
    }

    fn descriptor_mode(descriptor: &VmnetInterfaceDescriptor) -> u64 {
        let key = super::vmnet_operation_mode_key().expect("vmnet mode key should be available");

        // SAFETY: The descriptor owns a live XPC dictionary, and the key comes
        // from the vmnet SDK symbol wrapper.
        unsafe { super::xpc::xpc_dictionary_get_uint64(descriptor.dictionary.as_ptr(), key) }
    }

    fn descriptor_has_value(
        descriptor: &VmnetInterfaceDescriptor,
        key: *const std::ffi::c_char,
    ) -> bool {
        // SAFETY: The descriptor owns a live dictionary and the caller passes
        // a live vmnet key.
        !unsafe { super::xpc::xpc_dictionary_get_value(descriptor.dictionary.as_ptr(), key) }
            .is_null()
    }

    fn descriptor_bool(
        descriptor: &VmnetInterfaceDescriptor,
        key: *const std::ffi::c_char,
    ) -> bool {
        // SAFETY: The descriptor owns a live dictionary and the test only uses
        // keys populated with an XPC Boolean.
        unsafe { super::xpc::xpc_dictionary_get_bool(descriptor.dictionary.as_ptr(), key) }
    }

    fn descriptor_uint64(
        descriptor: &VmnetInterfaceDescriptor,
        key: *const std::ffi::c_char,
    ) -> u64 {
        // SAFETY: The descriptor owns a live dictionary and the test only uses
        // keys populated with an XPC uint64.
        unsafe { super::xpc::xpc_dictionary_get_uint64(descriptor.dictionary.as_ptr(), key) }
    }

    fn descriptor_string(
        descriptor: &VmnetInterfaceDescriptor,
        key: *const std::ffi::c_char,
    ) -> Option<String> {
        // SAFETY: The descriptor owns a live dictionary and the key comes from
        // vmnet. XPC owns the returned C string.
        let value =
            unsafe { super::xpc::xpc_dictionary_get_string(descriptor.dictionary.as_ptr(), key) };
        if value.is_null() {
            None
        } else {
            // SAFETY: XPC returned a non-null, NUL-terminated string borrowed
            // from the live descriptor dictionary.
            Some(
                unsafe { CStr::from_ptr(value) }
                    .to_str()
                    .expect("descriptor test string should be UTF-8")
                    .to_owned(),
            )
        }
    }

    fn descriptor_bridged_interface_name(descriptor: &VmnetInterfaceDescriptor) -> Option<String> {
        let key = super::vmnet_shared_interface_name_key()
            .expect("vmnet shared interface name key should be available");
        descriptor_string(descriptor, key)
    }

    fn descriptor_iov(descriptor: &VmnetPacketDescriptor) -> &libc::iovec {
        assert!(!descriptor.iov_ptr().is_null());

        // SAFETY: Tests only call this after descriptor construction, which
        // stores a pointer to a boxed iovec owned by the packet wrapper.
        unsafe { &*descriptor.iov_ptr() }
    }

    #[test]
    fn vmnet_modes_match_sdk_values() {
        assert_eq!(VmnetMode::Host.raw_value(), VMNET_HOST_MODE_VALUE);
        assert_eq!(VmnetMode::Shared.raw_value(), VMNET_SHARED_MODE_VALUE);
        assert_eq!(VmnetMode::Bridged.raw_value(), VMNET_BRIDGED_MODE_VALUE);
        assert_eq!(VmnetMode::Host.to_string(), "host");
        assert_eq!(VmnetMode::Shared.to_string(), "shared");
        assert_eq!(VmnetMode::Bridged.to_string(), "bridged");
    }

    #[test]
    fn vmnet_status_maps_known_and_unknown_values() {
        assert_eq!(VmnetStatus::from_raw(1000), VmnetStatus::Success);
        assert_eq!(VmnetStatus::from_raw(1010), VmnetStatus::NotAuthorized);
        assert_eq!(VmnetStatus::from_raw(9000), VmnetStatus::Unknown(9000));
        assert_eq!(
            VmnetStatus::NotAuthorized.to_string(),
            "VMNET_NOT_AUTHORIZED"
        );
        assert_eq!(
            VmnetStatus::Unknown(9000).to_string(),
            "unknown vmnet status"
        );
        assert!(!format!("{:?}", VmnetStatus::Unknown(9000)).contains("9000"));
    }

    #[test]
    fn vmnet_packet_descriptor_matches_sdk_layout() {
        assert_eq!(offset_of!(VmnetPacketDescriptor, vm_pkt_size), 0);
        assert_eq!(
            offset_of!(VmnetPacketDescriptor, vm_pkt_iov),
            size_of::<usize>()
        );
        assert_eq!(
            offset_of!(VmnetPacketDescriptor, vm_pkt_iovcnt),
            size_of::<usize>() + size_of::<*mut libc::iovec>()
        );
        assert_eq!(
            offset_of!(VmnetPacketDescriptor, vm_flags),
            size_of::<usize>() + size_of::<*mut libc::iovec>() + size_of::<u32>()
        );
        assert_eq!(align_of::<VmnetPacketDescriptor>(), align_of::<usize>());
        assert_eq!(
            size_of::<VmnetPacketDescriptor>(),
            size_of::<usize>() + size_of::<*mut libc::iovec>() + (2 * size_of::<u32>())
        );
    }

    #[test]
    fn packet_descriptor_debug_omits_buffers_sizes_and_pointers() {
        let packet = [0x12, 0x34, 0x56];
        let write = VmnetWritePacket::new(&packet)
            .expect("non-empty write packet descriptor should be created");
        let mut buffer = [0x78_u8; 2048];
        let read = VmnetReadPacket::new(&mut buffer)
            .expect("non-empty read packet descriptor should be created");

        assert_eq!(format!("{write:?}"), "VmnetWritePacket(<borrowed>)");
        assert_eq!(format!("{read:?}"), "VmnetReadPacket(<borrowed>)");
        assert_eq!(
            format!("{:?}", write.as_raw_descriptor()),
            "VmnetPacketDescriptor(<borrowed>)"
        );
        assert_eq!(
            format!("{:?}", read.as_raw_descriptor()),
            "VmnetPacketDescriptor(<borrowed>)"
        );
    }

    #[test]
    fn write_packet_descriptor_borrows_packet_buffer() {
        let packet = [0x12, 0x34, 0x56];
        let packet_ptr = packet.as_ptr().cast::<std::ffi::c_void>().cast_mut();
        let descriptor = VmnetWritePacket::new(&packet)
            .expect("non-empty write packet descriptor should be created");
        let raw_descriptor = descriptor.as_raw_descriptor();
        let iov = descriptor_iov(raw_descriptor);

        assert_eq!(raw_descriptor.packet_size(), packet.len());
        assert_eq!(raw_descriptor.iov_count(), 1);
        assert_eq!(raw_descriptor.flags(), 0);
        assert_eq!(
            raw_descriptor.iov_ptr(),
            descriptor.iov() as *const _ as *mut _
        );
        assert_eq!(iov.iov_base, packet_ptr);
        assert_eq!(iov.iov_len, packet.len());
    }

    #[test]
    fn read_packet_descriptor_borrows_read_buffer() {
        let mut buffer = [0_u8; 2048];
        let buffer_ptr = buffer.as_mut_ptr().cast::<std::ffi::c_void>();
        let buffer_len = buffer.len();
        let descriptor = VmnetReadPacket::new(&mut buffer)
            .expect("non-empty read packet descriptor should be created");
        let raw_descriptor = descriptor.as_raw_descriptor();
        let iov = descriptor_iov(raw_descriptor);

        assert_eq!(raw_descriptor.packet_size(), buffer_len);
        assert_eq!(raw_descriptor.iov_count(), 1);
        assert_eq!(raw_descriptor.flags(), 0);
        assert_eq!(
            raw_descriptor.iov_ptr(),
            descriptor.iov() as *const _ as *mut _
        );
        assert_eq!(iov.iov_base, buffer_ptr);
        assert_eq!(iov.iov_len, buffer_len);
    }

    #[test]
    fn packet_descriptors_reject_empty_buffers() {
        let write_error = VmnetWritePacket::new(&[])
            .expect_err("empty write packet descriptor should be rejected");
        let mut buffer = [];
        let read_error = VmnetReadPacket::new(&mut buffer)
            .expect_err("empty read packet descriptor should be rejected");

        assert_eq!(write_error, VmnetPacketDescriptorError::EmptyPacketBuffer);
        assert_eq!(read_error, VmnetPacketDescriptorError::EmptyPacketBuffer);
        assert_eq!(
            write_error.to_string(),
            "vmnet packet descriptor buffer must not be empty"
        );
    }

    #[test]
    fn write_packet_descriptor_iovec_pointer_survives_wrapper_move() {
        let packet = [0x7f, 0x45, 0x4c, 0x46];
        let packet_ptr = packet.as_ptr().cast::<std::ffi::c_void>().cast_mut();
        let descriptor = VmnetWritePacket::new(&packet)
            .expect("non-empty write packet descriptor should be created");
        let original_iov_ptr = descriptor.as_raw_descriptor().iov_ptr();

        let moved_descriptor = descriptor;
        let moved_iov = descriptor_iov(moved_descriptor.as_raw_descriptor());

        assert_eq!(
            moved_descriptor.as_raw_descriptor().iov_ptr(),
            original_iov_ptr
        );
        assert_eq!(moved_iov.iov_base, packet_ptr);
        assert_eq!(moved_iov.iov_len, packet.len());
    }

    #[test]
    fn read_packet_descriptor_iovec_pointer_survives_wrapper_move() {
        let mut buffer = [0_u8; 64];
        let buffer_ptr = buffer.as_mut_ptr().cast::<std::ffi::c_void>();
        let buffer_len = buffer.len();
        let descriptor = VmnetReadPacket::new(&mut buffer)
            .expect("non-empty read packet descriptor should be created");
        let original_iov_ptr = descriptor.as_raw_descriptor().iov_ptr();

        let moved_descriptor = descriptor;
        let moved_iov = descriptor_iov(moved_descriptor.as_raw_descriptor());

        assert_eq!(
            moved_descriptor.as_raw_descriptor().iov_ptr(),
            original_iov_ptr
        );
        assert_eq!(moved_iov.iov_base, buffer_ptr);
        assert_eq!(moved_iov.iov_len, buffer_len);
    }

    #[test]
    fn started_interface_builds_descriptor_and_stops_once() {
        let backend = RecordingVmnetBackend::new();
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::shared();
        let mut interface = StartedVmnetInterface::start(backend, &config)
            .expect("started vmnet interface should be created");

        assert!(interface.is_started());
        interface
            .stop()
            .expect("started vmnet interface should stop");
        assert!(!interface.is_started());
        interface
            .stop()
            .expect("second stop should be a no-op after cleanup");

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:shared".to_string(),
                format!("start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_interface_descriptor_failure_skips_backend_start() {
        let descriptor_error = VmnetInterfaceDescriptorError::MissingVmnetKey("test_key");
        let backend = RecordingVmnetBackend::new().with_descriptor_error(descriptor_error);
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::host();
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("descriptor failure should prevent interface ownership");

        assert_eq!(
            error,
            VmnetInterfaceStartError::Descriptor {
                source: descriptor_error
            }
        );
        assert_eq!(
            error
                .source()
                .expect("descriptor start error should preserve source")
                .to_string(),
            descriptor_error.to_string()
        );
        assert_eq!(recorded_events(&event_log), ["descriptor:host"]);
    }

    #[test]
    fn started_interface_start_failure_does_not_create_owner() {
        let backend = RecordingVmnetBackend::new().with_start_status(VmnetStatus::InvalidArgument);
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::host();
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("start failure should prevent interface ownership");

        match error {
            VmnetInterfaceStartError::Start { source, .. } => {
                assert_eq!(source.operation(), VmnetOperation::StartInterface);
                assert_eq!(source.status(), VmnetStatus::InvalidArgument);
            }
            VmnetInterfaceStartError::Descriptor { .. } => {
                panic!("start failure should not return a descriptor error");
            }
            VmnetInterfaceStartError::Parameters { .. } => {
                panic!("fake start failure should not return a parameter error");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:host".to_string(),
                format!("start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
            ]
        );
    }

    #[test]
    fn started_interface_failed_stop_marks_owner_uncertain_without_retry() {
        let backend = RecordingVmnetBackend::new().with_stop_status(VmnetStatus::SetupIncomplete);
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::host();
        let mut interface = StartedVmnetInterface::start(backend, &config)
            .expect("started vmnet interface should be created");
        let error = interface
            .stop()
            .expect_err("failed stop should return an error");

        assert_eq!(error.operation(), VmnetOperation::StopInterface);
        assert_eq!(error.status(), VmnetStatus::SetupIncomplete);
        assert!(!interface.is_started());
        assert!(interface.is_uncertain());

        interface
            .stop()
            .expect_err("uncertain interface must not retry stop");
        assert!(!interface.is_started());
        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:host".to_string(),
                format!("start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_interface_drop_stops_started_interface() {
        let backend = RecordingVmnetBackend::new();
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let _interface = StartedVmnetInterface::start(backend, &config)
                .expect("started vmnet interface should be created");
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:shared".to_string(),
                format!("start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_interface_drop_does_not_retry_after_failed_explicit_stop() {
        let backend = RecordingVmnetBackend::new().with_stop_status(VmnetStatus::SetupIncomplete);
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let mut interface = StartedVmnetInterface::start(backend, &config)
                .expect("started vmnet interface should be created");
            let error = interface
                .stop()
                .expect_err("first stop should return configured failure");

            assert_eq!(error.operation(), VmnetOperation::StopInterface);
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:shared".to_string(),
                format!("start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_packet_io_backend_starts_and_stops_on_drop() {
        let backend = RecordingVmnetBackend::new();
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::host();

        {
            let (backend, _interface) = StartedVmnetPacketIoBackend::start(backend, &config)
                .expect("started packet I/O backend should be created");
            assert!(backend.is_started());
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:host".to_string(),
                format!("start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_packet_io_backend_delegates_read_and_write() {
        let backend = RecordingVmnetBackend::new().with_read_result(Ok(Some(7)));
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let (mut backend, mut interface) = StartedVmnetPacketIoBackend::start(backend, &config)
                .expect("started packet I/O backend should be created");
            let mut read_buffer = [0_u8; 2048];
            let mut read_packet =
                VmnetReadPacket::new(&mut read_buffer).expect("read packet should build");
            let write_bytes = [0xaa, 0xbb, 0xcc];
            let mut write_packet =
                VmnetWritePacket::new(&write_bytes).expect("write packet should build");

            assert_eq!(
                backend
                    .read_packet(&mut interface, &mut read_packet)
                    .expect("read should delegate"),
                Some(7)
            );
            backend
                .write_packet(&mut interface, &mut write_packet)
                .expect("write should delegate");
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:shared".to_string(),
                format!("start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "read:9".to_string(),
                "write:9:3".to_string(),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_packet_io_backend_builds_virtio_packet_io_adapter() {
        let backend = RecordingVmnetBackend::new().with_read_result(Ok(Some(5)));
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let (backend, interface) = StartedVmnetPacketIoBackend::start(backend, &config)
                .expect("started packet I/O backend should be created");
            let mut packet_io =
                VmnetVirtioNetworkPacketIo::with_rx_buffer_len(backend, interface, 2048)
                    .expect("virtio vmnet packet I/O should build");
            let packet = packet_io
                .rx_source()
                .peek_packet()
                .expect("adapter RX should delegate")
                .expect("adapter RX packet should be present");

            assert_eq!(packet.bytes().len(), 5);
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:shared".to_string(),
                format!("start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "read:9".to_string(),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_packet_io_backend_preserves_write_failure() {
        let backend = RecordingVmnetBackend::new().with_write_result(Err(
            VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::WritePackets,
                expected: VmnetPacketCountExpectation::One,
                actual: 0,
            },
        ));
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::host();

        {
            let (mut backend, mut interface) = StartedVmnetPacketIoBackend::start(backend, &config)
                .expect("started packet I/O backend should be created");
            let write_bytes = [0xaa];
            let mut write_packet =
                VmnetWritePacket::new(&write_bytes).expect("write packet should build");
            let error = backend
                .write_packet(&mut interface, &mut write_packet)
                .expect_err("write failure should be preserved");

            assert_eq!(
                error,
                VmnetPacketIoError::UnexpectedPacketCount {
                    operation: VmnetOperation::WritePackets,
                    expected: VmnetPacketCountExpectation::One,
                    actual: 0,
                }
            );
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:host".to_string(),
                format!("start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "write:9:1".to_string(),
                "stop:9".to_string(),
            ]
        );
    }

    #[test]
    fn started_packet_io_backend_start_failure_does_not_create_owner() {
        let backend = RecordingVmnetBackend::new().with_start_status(VmnetStatus::NotAuthorized);
        let event_log = backend.events();
        let config = VmnetInterfaceConfig::host();
        let error = StartedVmnetPacketIoBackend::start(backend, &config)
            .expect_err("start failure should prevent packet I/O ownership");

        match error {
            VmnetInterfaceStartError::Start { source, .. } => {
                assert_eq!(source.operation(), VmnetOperation::StartInterface);
                assert_eq!(source.status(), VmnetStatus::NotAuthorized);
            }
            VmnetInterfaceStartError::Descriptor { .. } => {
                panic!("start failure should not return a descriptor error");
            }
            VmnetInterfaceStartError::Parameters { .. } => {
                panic!("fake start failure should not return a parameter error");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [
                "descriptor:host".to_string(),
                format!("start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_starts_and_stops_interface() {
        let api = RecordingVmnetSystemApi::new();
        let event_log = api.events();
        let config = VmnetInterfaceConfig::shared();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = StartedVmnetInterface::start(backend, &config)
            .expect("system vmnet interface should start");

        assert!(interface.is_started());
        interface
            .stop()
            .expect("system vmnet interface should stop");
        assert!(!interface.is_started());
        interface
            .stop()
            .expect("second system stop should be a no-op");

        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_publishes_packet_event_hint_and_drains_before_stop() {
        let api = RecordingVmnetSystemApi::new().with_packet_event(Some(7));
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let published = Arc::new(Mutex::new(Vec::new()));
        let callback_published = Arc::clone(&published);
        let callback = VmnetPacketAvailableCallback::new(move |estimate| {
            callback_published
                .lock()
                .expect("packet event result should lock")
                .push(estimate);
        });

        backend
            .enable_packet_available_callback(&mut interface, callback)
            .expect("packet callback enable should succeed");
        backend
            .stop_interface(&mut interface)
            .expect("stop should disable, drain, then stop");

        assert_eq!(
            *published.lock().expect("packet event result should lock"),
            [Some(7)]
        );
        assert_eq!(
            recorded_events(&event_log),
            [
                "system-events:20:enable:1",
                "system-events:20:disable:1",
                "system-stop:20",
            ]
        );
        assert!(interface.packet_event_callback.is_none());
    }

    #[test]
    fn system_vmnet_backend_disable_failure_prevents_stop_and_retains_callback_owner() {
        let api = RecordingVmnetSystemApi::new()
            .with_event_callback_status(VmnetStatus::Success)
            .with_event_callback_status(VmnetStatus::Failure);
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        backend
            .enable_packet_available_callback(
                &mut interface,
                VmnetPacketAvailableCallback::new(|_| {}),
            )
            .expect("packet callback enable should succeed");

        let error = backend
            .stop_interface(&mut interface)
            .expect_err("failed callback disable must prevent vmnet stop");

        assert_eq!(error.operation(), VmnetOperation::DisablePacketEvents);
        assert!(interface.packet_event_callback.is_some());
        assert_eq!(
            recorded_events(&event_log),
            ["system-events:20:enable:1", "system-events:20:disable:1"]
        );
    }

    #[test]
    fn started_packet_io_backend_never_retries_uncertain_event_disable() {
        let api = RecordingVmnetSystemApi::new()
            .with_event_callback_status(VmnetStatus::Success)
            .with_event_callback_status(VmnetStatus::Failure);
        let event_log = api.events();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let config = VmnetInterfaceConfig::host();
        let (mut backend, _interface) = StartedVmnetPacketIoBackend::start(backend, &config)
            .expect("system packet-I/O backend should start");
        backend
            .enable_packet_available_callback(VmnetPacketAvailableCallback::new(|_| {}))
            .expect("packet callback enable should succeed");

        let first_error = backend
            .stop()
            .expect_err("failed callback disable must make cleanup uncertain");
        assert_eq!(first_error.operation(), VmnetOperation::DisablePacketEvents);
        let second_error = backend
            .stop()
            .expect_err("uncertain cleanup must not be retried explicitly");
        assert_eq!(
            second_error.operation(),
            VmnetOperation::DisablePacketEvents
        );
        drop(backend);

        let events = recorded_events(&event_log);
        assert_eq!(
            events
                .iter()
                .filter(|event| event.as_str() == "system-events:10:disable:1")
                .count(),
            1,
            "explicit retry and drop must not issue another disable"
        );
        assert!(
            !events.iter().any(|event| event == "system-stop:10"),
            "an uncertain callback retirement must prevent vmnet stop"
        );
    }

    #[test]
    fn system_vmnet_backend_cleans_up_after_start_completion_failure() {
        let api = RecordingVmnetSystemApi::new().with_start_completion(VmnetStatus::InvalidAccess);
        let event_log = api.events();
        let config = VmnetInterfaceConfig::host();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("system start completion failure should prevent ownership");

        match error {
            VmnetInterfaceStartError::Start { source, .. } => {
                assert_eq!(source.operation(), VmnetOperation::StartInterface);
                assert_eq!(source.status(), VmnetStatus::InvalidAccess);
            }
            VmnetInterfaceStartError::Descriptor { .. } => {
                panic!("system start failure should not be reported as a descriptor error");
            }
            VmnetInterfaceStartError::Parameters { .. } => {
                panic!("system service failure should not be reported as a parameter error");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_cleans_up_after_parameter_validation_failure() {
        let api = RecordingVmnetSystemApi::new().with_missing_start_mtu();
        let event_log = api.events();
        let config = VmnetInterfaceConfig::host();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("missing successful-start MTU should prevent ownership");

        match error {
            VmnetInterfaceStartError::Parameters {
                source,
                disposition,
            } => {
                assert_eq!(source.field(), VmnetInterfaceParameterField::EffectiveMtu);
                assert_eq!(source.problem(), VmnetInterfaceParameterProblem::Missing);
                assert_eq!(disposition, VmnetInterfaceStartDisposition::Retryable);
            }
            VmnetInterfaceStartError::Descriptor { .. }
            | VmnetInterfaceStartError::Start { .. } => {
                panic!("missing successful-start MTU should be a parameter error");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_parameter_failure_is_terminal_when_cleanup_is_uncertain() {
        let api = RecordingVmnetSystemApi::new()
            .with_missing_start_mtu()
            .with_stop_schedule_status(VmnetStatus::SetupIncomplete);
        let event_log = api.events();
        let config = VmnetInterfaceConfig::shared();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("unclean parameter failure should prevent ownership");

        assert_eq!(
            error.disposition(),
            VmnetInterfaceStartDisposition::Terminal
        );
        assert!(matches!(
            error,
            VmnetInterfaceStartError::Parameters {
                source,
                disposition: VmnetInterfaceStartDisposition::Terminal,
            } if source.field() == VmnetInterfaceParameterField::EffectiveMtu
        ));
        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_rejects_null_start_handle() {
        let api = RecordingVmnetSystemApi::new()
            .with_null_start_handle()
            .with_start_completion(VmnetStatus::NotAuthorized);
        let event_log = api.events();
        let config = VmnetInterfaceConfig::host();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("null vmnet start handle should prevent ownership");

        match error {
            VmnetInterfaceStartError::Start { source, .. } => {
                assert_eq!(source.operation(), VmnetOperation::StartInterface);
                assert_eq!(source.status(), VmnetStatus::NotAuthorized);
            }
            VmnetInterfaceStartError::Descriptor { .. } => {
                panic!("null start handle should not be reported as a descriptor error");
            }
            VmnetInterfaceStartError::Parameters { .. } => {
                panic!("null start handle should not be reported as a parameter error");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [format!("system-start:{}", u64::from(VMNET_HOST_MODE_VALUE))]
        );
    }

    #[test]
    fn system_vmnet_backend_maps_successful_null_start_handle_to_failure() {
        let api = RecordingVmnetSystemApi::new().with_null_start_handle();
        let event_log = api.events();
        let config = VmnetInterfaceConfig::shared();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("null vmnet start handle should fail even with success completion");

        match error {
            VmnetInterfaceStartError::Start { source, .. } => {
                assert_eq!(source.operation(), VmnetOperation::StartInterface);
                assert_eq!(source.status(), VmnetStatus::Failure);
            }
            VmnetInterfaceStartError::Descriptor { .. } => {
                panic!("null start handle should not be reported as a descriptor error");
            }
            VmnetInterfaceStartError::Parameters { .. } => {
                panic!("null start handle should not be reported as a parameter error");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [format!(
                "system-start:{}",
                u64::from(VMNET_SHARED_MODE_VALUE)
            )]
        );
    }

    #[test]
    fn system_vmnet_backend_failed_stop_schedule_marks_interface_uncertain() {
        let api =
            RecordingVmnetSystemApi::new().with_stop_schedule_status(VmnetStatus::SetupIncomplete);
        let event_log = api.events();
        let config = VmnetInterfaceConfig::host();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = StartedVmnetInterface::start(backend, &config)
            .expect("system vmnet interface should start");
        let error = interface
            .stop()
            .expect_err("failed system stop schedule should return an error");

        assert_eq!(error.operation(), VmnetOperation::StopInterface);
        assert_eq!(error.status(), VmnetStatus::SetupIncomplete);
        assert!(!interface.is_started());
        assert!(interface.is_uncertain());

        let second = interface
            .stop()
            .expect_err("uncertain system stop must not be retried");
        assert_eq!(second.status(), VmnetStatus::Failure);
        assert!(!interface.is_started());
        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_failed_stop_completion_marks_interface_uncertain() {
        let api = RecordingVmnetSystemApi::new()
            .with_stop_completion_status(VmnetStatus::SharingServiceBusy);
        let event_log = api.events();
        let config = VmnetInterfaceConfig::shared();
        let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = StartedVmnetInterface::start(backend, &config)
            .expect("system vmnet interface should start");
        let error = interface
            .stop()
            .expect_err("failed system stop completion should return an error");

        assert_eq!(error.operation(), VmnetOperation::StopInterface);
        assert_eq!(error.status(), VmnetStatus::SharingServiceBusy);
        assert!(!interface.is_started());
        assert!(interface.is_uncertain());

        let second = interface
            .stop()
            .expect_err("uncertain system stop must not be retried");
        assert_eq!(second.status(), VmnetStatus::Failure);
        assert!(!interface.is_started());
        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_start_timeout_cleans_up_with_finite_deadline() {
        let api = RecordingVmnetSystemApi::new().without_start_completion();
        let event_log = api.events();
        let config = VmnetInterfaceConfig::host();
        let backend =
            super::SystemVmnetInterfaceBackendWithApi::with_api_and_timeout(api, Duration::ZERO);
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("start timeout should prevent ownership");

        match error {
            VmnetInterfaceStartError::Start {
                source,
                disposition,
            } => {
                assert_eq!(
                    source.completion_error(),
                    Some(super::VmnetCompletionError::TimedOut)
                );
                assert_eq!(disposition, VmnetInterfaceStartDisposition::Retryable);
            }
            VmnetInterfaceStartError::Descriptor { .. }
            | VmnetInterfaceStartError::Parameters { .. } => {
                panic!("start timeout should remain a lifecycle error");
            }
        }
        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_start_timeout_is_terminal_when_cleanup_times_out() {
        let api = RecordingVmnetSystemApi::new()
            .without_start_completion()
            .without_stop_completion();
        let event_log = api.events();
        let config = VmnetInterfaceConfig::host();
        let backend =
            super::SystemVmnetInterfaceBackendWithApi::with_api_and_timeout(api, Duration::ZERO);
        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("start and cleanup timeout should prevent ownership");

        assert_eq!(
            error.disposition(),
            VmnetInterfaceStartDisposition::Terminal
        );
        assert!(matches!(
            error,
            VmnetInterfaceStartError::Start {
                source,
                disposition: VmnetInterfaceStartDisposition::Terminal,
            } if source.completion_error() == Some(super::VmnetCompletionError::TimedOut)
        ));
        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_distinguishes_lost_start_callback_owner() {
        let api = RecordingVmnetSystemApi::new().without_start_completion();
        let event_log = api.events();
        let config = VmnetInterfaceConfig::host();
        let backend =
            super::SystemVmnetInterfaceBackendWithApi::with_api_timeout_and_channel_liveness(
                api,
                Duration::ZERO,
                false,
            );

        let error = StartedVmnetInterface::start(backend, &config)
            .expect_err("lost start callback owner should prevent ownership");

        assert!(matches!(
            error,
            VmnetInterfaceStartError::Start {
                source,
                disposition: VmnetInterfaceStartDisposition::Retryable,
            } if source.completion_error() == Some(super::VmnetCompletionError::ChannelClosed)
        ));
        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_HOST_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn bounded_completion_distinguishes_channel_loss_and_rejects_late_send() {
        let (sender, receiver) = std::sync::mpsc::channel::<u8>();
        drop(sender);
        let channel_error = super::wait_for_vmnet_completion(
            receiver,
            VmnetOperation::StartInterface,
            Duration::ZERO,
        )
        .expect_err("closed completion channel should fail");
        assert_eq!(
            channel_error.completion_error(),
            Some(super::VmnetCompletionError::ChannelClosed)
        );

        let (sender, receiver) = std::sync::mpsc::channel::<u8>();
        let timeout_error = super::wait_for_vmnet_completion(
            receiver,
            VmnetOperation::StartInterface,
            Duration::ZERO,
        )
        .expect_err("undelivered completion should time out");
        assert_eq!(
            timeout_error.completion_error(),
            Some(super::VmnetCompletionError::TimedOut)
        );
        assert!(
            sender.send(1).is_err(),
            "a late completion must not regain an owner after timeout"
        );
    }

    #[test]
    fn system_vmnet_backend_stop_timeout_is_not_retried() {
        let api = RecordingVmnetSystemApi::new().without_stop_completion();
        let event_log = api.events();
        let config = VmnetInterfaceConfig::shared();
        let backend =
            super::SystemVmnetInterfaceBackendWithApi::with_api_and_timeout(api, Duration::ZERO);
        let mut interface = StartedVmnetInterface::start(backend, &config)
            .expect("system vmnet interface should start");

        let error = interface.stop().expect_err("stop timeout should fail");
        assert_eq!(
            error.completion_error(),
            Some(super::VmnetCompletionError::TimedOut)
        );
        assert!(interface.is_uncertain());
        interface
            .stop()
            .expect_err("uncertain system stop must not be retried");
        drop(interface);
        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_drop_stops_started_interface() {
        let api = RecordingVmnetSystemApi::new();
        let event_log = api.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
            let _interface = StartedVmnetInterface::start(backend, &config)
                .expect("system vmnet interface should start");
        }

        assert_eq!(
            recorded_events(&event_log),
            [
                format!("system-start:{}", u64::from(VMNET_SHARED_MODE_VALUE)),
                "system-stop:10".to_string(),
            ]
        );
    }

    #[test]
    fn system_vmnet_backend_reads_single_packet() {
        let api = RecordingVmnetSystemApi::new().with_read_result(VmnetStatus::Success, 1, 128);
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let mut buffer = [0_u8; 2048];
        let mut packet =
            VmnetReadPacket::new(&mut buffer).expect("read packet descriptor should be valid");

        let packet_size = backend
            .read_packet(&mut interface, &mut packet)
            .expect("system vmnet read should succeed");

        assert_eq!(packet_size, Some(128));
        assert_eq!(packet.as_raw_descriptor().packet_size(), 128);
        assert_eq!(recorded_events(&event_log), ["system-read:20:1"]);
    }

    #[test]
    fn system_vmnet_backend_reads_one_bounded_descriptor_array() {
        let api = RecordingVmnetSystemApi::new().with_read_result(VmnetStatus::Success, 2, 96);
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let mut buffer = [0_u8; 384];
        let mut packet_lengths = [0_usize; 3];

        let completed = backend
            .read_packet_batch(&mut interface, &mut buffer, 128, 3, &mut packet_lengths)
            .expect("bounded vmnet read batch should succeed");

        assert_eq!(completed, 2);
        assert_eq!(packet_lengths, [96, 96, 0]);
        assert_eq!(recorded_events(&event_log), ["system-read:20:3"]);
    }

    #[test]
    fn system_vmnet_backend_rejects_untrusted_batch_read_count() {
        for returned in [-1, 4] {
            let api =
                RecordingVmnetSystemApi::new().with_read_result(VmnetStatus::Success, returned, 64);
            let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
            let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
            let mut buffer = [0_u8; 384];
            let mut packet_lengths = [0_usize; 3];

            let error = backend
                .read_packet_batch(&mut interface, &mut buffer, 128, 3, &mut packet_lengths)
                .expect_err("out-of-range vmnet read count must fail closed");

            assert!(matches!(
                error,
                VmnetPacketIoError::UnexpectedPacketCount {
                    operation: VmnetOperation::ReadPackets,
                    expected: VmnetPacketCountExpectation::AtMost(3),
                    actual,
                } if actual == returned
            ));
            assert_eq!(packet_lengths, [0, 0, 0]);
        }
    }

    #[test]
    fn system_vmnet_backend_returns_none_when_no_packet_is_available() {
        let api = RecordingVmnetSystemApi::new().with_read_result(VmnetStatus::Success, 0, 64);
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let mut buffer = [0_u8; 2048];
        let mut packet =
            VmnetReadPacket::new(&mut buffer).expect("read packet descriptor should be valid");

        let packet_size = backend
            .read_packet(&mut interface, &mut packet)
            .expect("system vmnet read should succeed without packets");

        assert_eq!(packet_size, None);
        assert_eq!(recorded_events(&event_log), ["system-read:20:1"]);
    }

    #[test]
    fn system_vmnet_backend_preserves_read_failure_status() {
        let api =
            RecordingVmnetSystemApi::new().with_read_result(VmnetStatus::BufferExhausted, 0, 64);
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let mut buffer = [0_u8; 2048];
        let mut packet =
            VmnetReadPacket::new(&mut buffer).expect("read packet descriptor should be valid");
        let error = backend
            .read_packet(&mut interface, &mut packet)
            .expect_err("read failure status should be preserved");

        match error {
            VmnetPacketIoError::Vmnet { source } => {
                assert_eq!(source.operation(), VmnetOperation::ReadPackets);
                assert_eq!(source.status(), VmnetStatus::BufferExhausted);
            }
            VmnetPacketIoError::InterfaceStopped => {
                panic!("vmnet read status failure should not become a stopped interface error");
            }
            VmnetPacketIoError::UnexpectedPacketCount { .. } => {
                panic!("vmnet read status failure should not become a count error");
            }
            VmnetPacketIoError::ReadPacketSizeExceedsBuffer { .. } => {
                panic!("vmnet read status failure should not become a packet size error");
            }
            VmnetPacketIoError::InvalidBatch { .. } => {
                panic!("vmnet read status failure should not become a batch error");
            }
        }
        assert_eq!(recorded_events(&event_log), ["system-read:20:1"]);
    }

    #[test]
    fn system_vmnet_backend_rejects_unexpected_read_packet_count() {
        let api = RecordingVmnetSystemApi::new().with_read_result(VmnetStatus::Success, -1, 64);
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let mut buffer = [0_u8; 2048];
        let mut packet =
            VmnetReadPacket::new(&mut buffer).expect("read packet descriptor should be valid");
        let error = backend
            .read_packet(&mut interface, &mut packet)
            .expect_err("unexpected read packet count should fail");

        assert_eq!(
            error,
            VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::ReadPackets,
                expected: VmnetPacketCountExpectation::ZeroOrOne,
                actual: -1,
            }
        );
        assert_eq!(
            error.to_string(),
            "vmnet_read returned an unexpected packet count"
        );
    }

    #[test]
    fn system_vmnet_backend_rejects_oversized_read_packet() {
        let api = RecordingVmnetSystemApi::new().with_read_result(VmnetStatus::Success, 1, 2049);
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let mut buffer = [0_u8; 2048];
        let mut packet =
            VmnetReadPacket::new(&mut buffer).expect("read packet descriptor should be valid");
        let error = backend
            .read_packet(&mut interface, &mut packet)
            .expect_err("oversized read packet should fail");

        assert_eq!(
            error,
            VmnetPacketIoError::ReadPacketSizeExceedsBuffer {
                packet_size: 2049,
                buffer_len: 2048,
            }
        );
        assert_eq!(
            error.to_string(),
            "vmnet_read returned a packet larger than the validated read buffer"
        );
        let debug = format!("{error:?}");
        assert!(!debug.contains("2049"));
        assert!(!debug.contains("2048"));
    }

    #[test]
    fn system_vmnet_backend_writes_single_packet() {
        let api = RecordingVmnetSystemApi::new();
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let packet = [0xde, 0xad, 0xbe, 0xef];
        let mut packet =
            VmnetWritePacket::new(&packet).expect("write packet descriptor should be valid");

        backend
            .write_packet(&mut interface, &mut packet)
            .expect("system vmnet write should succeed");

        assert_eq!(recorded_events(&event_log), ["system-write:20:1"]);
    }

    #[test]
    fn system_vmnet_backend_reports_successful_short_batch_write_prefix() {
        let api = RecordingVmnetSystemApi::new().with_write_result(VmnetStatus::Success, 2);
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let bytes = [1_u8, 2, 3, 4, 5, 6];
        let ranges = [0..2, 2..4, 4..6];

        let completed = backend
            .write_packet_batch(&mut interface, &bytes, &ranges)
            .expect("short vmnet write is an explicit completed prefix");

        assert_eq!(completed, 2);
        assert_eq!(recorded_events(&event_log), ["system-write:20:3"]);
    }

    #[test]
    fn system_vmnet_backend_preserves_write_failure_status() {
        let api = RecordingVmnetSystemApi::new().with_write_result(VmnetStatus::PacketTooBig, 0);
        let event_log = api.events();
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let packet = [0xde, 0xad, 0xbe, 0xef];
        let mut packet =
            VmnetWritePacket::new(&packet).expect("write packet descriptor should be valid");
        let error = backend
            .write_packet(&mut interface, &mut packet)
            .expect_err("write failure status should be preserved");

        match error {
            VmnetPacketIoError::Vmnet { source } => {
                assert_eq!(source.operation(), VmnetOperation::WritePackets);
                assert_eq!(source.status(), VmnetStatus::PacketTooBig);
            }
            VmnetPacketIoError::InterfaceStopped => {
                panic!("vmnet write status failure should not become a stopped interface error");
            }
            VmnetPacketIoError::UnexpectedPacketCount { .. } => {
                panic!("vmnet write status failure should not become a count error");
            }
            VmnetPacketIoError::ReadPacketSizeExceedsBuffer { .. } => {
                panic!("vmnet write status failure should not become a packet size error");
            }
            VmnetPacketIoError::InvalidBatch { .. } => {
                panic!("vmnet write status failure should not become a batch error");
            }
        }
        assert_eq!(recorded_events(&event_log), ["system-write:20:1"]);
    }

    #[test]
    fn system_vmnet_backend_rejects_unexpected_write_packet_count() {
        let api = RecordingVmnetSystemApi::new().with_write_result(VmnetStatus::Success, 2);
        let mut backend = super::SystemVmnetInterfaceBackendWithApi::with_api(api);
        let mut interface = super::SystemVmnetInterface::new(fake_interface(0x20));
        let packet = [0xde, 0xad, 0xbe, 0xef];
        let mut packet =
            VmnetWritePacket::new(&packet).expect("write packet descriptor should be valid");
        let error = backend
            .write_packet(&mut interface, &mut packet)
            .expect_err("unexpected write packet count should fail");

        assert_eq!(
            error,
            VmnetPacketIoError::UnexpectedPacketCount {
                operation: VmnetOperation::WritePackets,
                expected: VmnetPacketCountExpectation::One,
                actual: 2,
            }
        );
        assert_eq!(
            error.to_string(),
            "vmnet_write returned an unexpected packet count"
        );
    }

    #[test]
    fn vmnet_error_includes_operation_and_status() {
        let error = VmnetError::new(VmnetOperation::ReadPackets, VmnetStatus::BufferExhausted);

        assert_eq!(error.operation(), VmnetOperation::ReadPackets);
        assert_eq!(error.status(), VmnetStatus::BufferExhausted);
        assert_eq!(
            error.to_string(),
            "vmnet_read failed with VMNET_BUFFER_EXHAUSTED"
        );
    }

    #[test]
    fn vmnet_interface_config_models_modes() {
        let host = VmnetInterfaceConfig::host();
        let shared = VmnetInterfaceConfig::shared();
        let bridged = VmnetInterfaceConfig::bridged("en0").expect("bridged config should validate");

        assert_eq!(host.mode(), VmnetMode::Host);
        assert_eq!(host.bridged_interface_name(), None);
        assert_eq!(shared.mode(), VmnetMode::Shared);
        assert_eq!(shared.bridged_interface_name(), None);
        assert_eq!(bridged.mode(), VmnetMode::Bridged);
        assert_eq!(bridged.bridged_interface_name(), Some("en0"));
    }

    #[test]
    fn vmnet_host_dev_name_maps_host_mode() {
        let config = VmnetInterfaceConfig::from_host_dev_name("vmnet:host")
            .expect("host host_dev_name should map");

        assert_eq!(config.mode(), VmnetMode::Host);
        assert_eq!(config.bridged_interface_name(), None);
    }

    #[test]
    fn vmnet_host_dev_name_maps_shared_mode() {
        let config = VmnetInterfaceConfig::from_host_dev_name("vmnet:shared")
            .expect("shared host_dev_name should map");

        assert_eq!(config.mode(), VmnetMode::Shared);
        assert_eq!(config.bridged_interface_name(), None);
    }

    #[test]
    fn vmnet_host_dev_name_maps_bridged_mode() {
        let config = VmnetInterfaceConfig::from_host_dev_name("vmnet:bridged:en0")
            .expect("bridged host_dev_name should map");

        assert_eq!(config.mode(), VmnetMode::Bridged);
        assert_eq!(config.bridged_interface_name(), Some("en0"));
    }

    #[test]
    fn vmnet_host_dev_name_rejects_unsupported_bare_name() {
        let error = VmnetInterfaceConfig::from_host_dev_name("tap0")
            .expect_err("bare host_dev_name should be rejected by vmnet mapping");

        assert_eq!(
            error,
            VmnetHostDeviceNameConfigError::UnsupportedHostDeviceName
        );
        assert!(error.source().is_none());
        assert_eq!(
            error.to_string(),
            "unsupported vmnet host_dev_name; expected vmnet:host, vmnet:shared, or vmnet:bridged:<interface>"
        );
    }

    #[test]
    fn vmnet_host_dev_name_rejects_empty_bridged_interface() {
        let error = VmnetInterfaceConfig::from_host_dev_name("vmnet:bridged:")
            .expect_err("empty bridged interface should be rejected");

        assert_eq!(
            error,
            VmnetHostDeviceNameConfigError::BridgedInterface {
                source: VmnetInterfaceConfigError::EmptyBridgedInterfaceName
            }
        );
        assert_eq!(
            error
                .source()
                .expect("bridged error should expose source")
                .to_string(),
            "vmnet bridged interface name must not be empty"
        );
    }

    #[test]
    fn vmnet_host_dev_name_rejects_interior_nul_bridged_interface() {
        let error = VmnetInterfaceConfig::from_host_dev_name("vmnet:bridged:en\0")
            .expect_err("interior NUL bridged interface should be rejected");

        assert_eq!(
            error,
            VmnetHostDeviceNameConfigError::BridgedInterface {
                source: VmnetInterfaceConfigError::InteriorNulInBridgedInterfaceName
            }
        );
        assert_eq!(
            error
                .source()
                .expect("bridged error should expose source")
                .to_string(),
            "vmnet bridged interface name must not contain NUL bytes"
        );
    }

    #[test]
    fn vmnet_host_dev_name_rejects_control_character_bridged_interface() {
        let error = VmnetInterfaceConfig::from_host_dev_name("vmnet:bridged:en\n0")
            .expect_err("control-character bridged interface should be rejected");

        assert_eq!(
            error,
            VmnetHostDeviceNameConfigError::BridgedInterface {
                source: VmnetInterfaceConfigError::ControlCharacterInBridgedInterfaceName
            }
        );
        assert_eq!(
            error
                .source()
                .expect("bridged error should expose source")
                .to_string(),
            "vmnet bridged interface name must not contain ASCII control characters"
        );
        assert!(!error.to_string().contains("en\n0"));
    }

    #[test]
    fn bridged_config_rejects_empty_interface_name() {
        let error = VmnetInterfaceConfig::bridged("")
            .expect_err("empty bridged interface should be rejected");

        assert_eq!(error, VmnetInterfaceConfigError::EmptyBridgedInterfaceName);
        assert_eq!(
            error.to_string(),
            "vmnet bridged interface name must not be empty"
        );
    }

    #[test]
    fn bridged_config_rejects_interior_nul_interface_name() {
        let error = VmnetInterfaceConfig::bridged("en\0")
            .expect_err("interior NUL bridged interface should be rejected");

        assert_eq!(
            error,
            VmnetInterfaceConfigError::InteriorNulInBridgedInterfaceName
        );
        assert_eq!(
            error.to_string(),
            "vmnet bridged interface name must not contain NUL bytes"
        );
    }

    #[test]
    fn bridged_config_rejects_control_character_interface_name() {
        let error = VmnetInterfaceConfig::bridged("en\t0")
            .expect_err("control-character bridged interface should be rejected");

        assert_eq!(
            error,
            VmnetInterfaceConfigError::ControlCharacterInBridgedInterfaceName
        );
        assert_eq!(
            error.to_string(),
            "vmnet bridged interface name must not contain ASCII control characters"
        );
    }

    #[test]
    fn vmnet_interface_descriptor_models_host_mode() {
        let config = VmnetInterfaceConfig::host();
        let descriptor =
            VmnetInterfaceDescriptor::new(&config).expect("host descriptor should be created");

        assert_eq!(
            descriptor_mode(&descriptor),
            u64::from(VMNET_HOST_MODE_VALUE)
        );
        assert_eq!(descriptor_bridged_interface_name(&descriptor), None);
    }

    #[test]
    fn vmnet_interface_descriptor_models_shared_mode() {
        let config = VmnetInterfaceConfig::shared();
        let descriptor =
            VmnetInterfaceDescriptor::new(&config).expect("shared descriptor should be created");

        assert_eq!(
            descriptor_mode(&descriptor),
            u64::from(VMNET_SHARED_MODE_VALUE)
        );
        assert_eq!(descriptor_bridged_interface_name(&descriptor), None);
    }

    #[test]
    fn vmnet_interface_descriptor_models_bridged_mode() {
        let config = VmnetInterfaceConfig::bridged("en0").expect("bridged config should validate");
        let descriptor =
            VmnetInterfaceDescriptor::new(&config).expect("bridged descriptor should be created");

        assert_eq!(
            descriptor_mode(&descriptor),
            u64::from(VMNET_BRIDGED_MODE_VALUE)
        );
        assert_eq!(
            descriptor_bridged_interface_name(&descriptor),
            Some("en0".to_string())
        );
    }

    #[test]
    fn vmnet_interface_descriptor_encodes_requested_identity_and_redacts_private_values() {
        let configured_mac = GuestMacAddress::from_bytes([0x01, 0x23, 0x45, 0x67, 0x89, 0xab]);
        let configured = VmnetInterfaceConfig::shared()
            .with_guest_mac(Some(configured_mac))
            .with_mtu(Some(1400));
        let configured_descriptor = VmnetInterfaceDescriptor::new(&configured)
            .expect("configured descriptor should be created");
        let allocate_key =
            super::vmnet_allocate_mac_address_key().expect("allocate-MAC key should be available");
        let mac_key = super::vmnet_mac_address_key().expect("MAC key should be available");
        let mtu_key = super::vmnet_mtu_key().expect("MTU key should be available");

        assert!(!descriptor_bool(&configured_descriptor, allocate_key));
        assert_eq!(
            descriptor_string(&configured_descriptor, mac_key),
            Some(configured_mac.to_string())
        );
        assert_eq!(descriptor_uint64(&configured_descriptor, mtu_key), 1400);

        let allocated_descriptor = VmnetInterfaceDescriptor::new(&VmnetInterfaceConfig::host())
            .expect("allocated descriptor should be created");
        assert!(descriptor_bool(&allocated_descriptor, allocate_key));
        assert!(!descriptor_has_value(&allocated_descriptor, mac_key));

        let bridged = VmnetInterfaceConfig::bridged("private-en7")
            .expect("bridged config should validate")
            .with_mtu(Some(1300));
        let bridged_descriptor =
            VmnetInterfaceDescriptor::new(&bridged).expect("bridged descriptor should be created");
        assert!(!descriptor_has_value(&bridged_descriptor, mtu_key));
        assert_eq!(bridged_descriptor.result_policy.requested_mtu, Some(1300));

        if let Some(direct_header_key) = super::optional_vmnet_enable_virtio_header_key() {
            assert!(descriptor_has_value(
                &configured_descriptor,
                direct_header_key
            ));
            assert!(descriptor_bool(&configured_descriptor, direct_header_key));
        }

        let debug = format!("{configured:?} {configured_descriptor:?} {bridged_descriptor:?}");
        assert!(debug.contains("<configured>"));
        assert!(!debug.contains(&configured_mac.to_string()));
        assert!(!debug.contains("private-en7"));
        assert!(!debug.contains("1400"));
        assert!(!debug.contains("1300"));
    }

    #[test]
    fn vmnet_result_decoder_requires_dictionary_and_core_integer_fields() {
        assert_parameter_error(
            super::decode_vmnet_interface_parameters(ptr::null_mut()),
            VmnetInterfaceParameterField::ResultDictionary,
            VmnetInterfaceParameterProblem::Missing,
        );

        let wrong_container =
            test_result_dictionary(Some("02:00:00:00:00:10"), Some(1500), Some(2048));
        let mac_key = super::vmnet_mac_address_key().expect("MAC key should exist");
        // SAFETY: The dictionary and key are live; the returned value remains
        // borrowed for this synchronous decode attempt.
        let string_object =
            unsafe { super::xpc::xpc_dictionary_get_value(wrong_container.as_ptr(), mac_key) };
        assert_parameter_error(
            super::decode_vmnet_interface_parameters(string_object),
            VmnetInterfaceParameterField::ResultDictionary,
            VmnetInterfaceParameterProblem::WrongType,
        );

        let missing_mtu = test_result_dictionary(Some("02:00:00:00:00:10"), None, Some(2048));
        assert_parameter_error(
            super::decode_vmnet_interface_parameters(missing_mtu.as_ptr()),
            VmnetInterfaceParameterField::EffectiveMtu,
            VmnetInterfaceParameterProblem::Missing,
        );
        let wrong_mtu = test_result_dictionary(Some("02:00:00:00:00:10"), Some(1500), Some(2048));
        set_dictionary_string(
            &wrong_mtu,
            super::vmnet_mtu_key().expect("MTU key should exist"),
            "1500",
        );
        assert_parameter_error(
            super::decode_vmnet_interface_parameters(wrong_mtu.as_ptr()),
            VmnetInterfaceParameterField::EffectiveMtu,
            VmnetInterfaceParameterProblem::WrongType,
        );

        let missing_maximum = test_result_dictionary(Some("02:00:00:00:00:10"), Some(1500), None);
        assert_parameter_error(
            super::decode_vmnet_interface_parameters(missing_maximum.as_ptr()),
            VmnetInterfaceParameterField::MaximumPacketSize,
            VmnetInterfaceParameterProblem::Missing,
        );
        let wrong_maximum =
            test_result_dictionary(Some("02:00:00:00:00:10"), Some(1500), Some(2048));
        set_dictionary_string(
            &wrong_maximum,
            super::vmnet_max_packet_size_key().expect("maximum-packet key should exist"),
            "2048",
        );
        assert_parameter_error(
            super::decode_vmnet_interface_parameters(wrong_maximum.as_ptr()),
            VmnetInterfaceParameterField::MaximumPacketSize,
            VmnetInterfaceParameterProblem::WrongType,
        );
    }

    #[test]
    fn vmnet_result_decoder_rejects_wrong_and_malformed_mac_values() {
        let wrong_type = test_result_dictionary(Some("02:00:00:00:00:10"), Some(1500), Some(2048));
        set_dictionary_uint64(
            &wrong_type,
            super::vmnet_mac_address_key().expect("MAC key should exist"),
            1,
        );
        assert_parameter_error(
            super::decode_vmnet_interface_parameters(wrong_type.as_ptr()),
            VmnetInterfaceParameterField::MacAddress,
            VmnetInterfaceParameterProblem::WrongType,
        );

        for malformed in ["02:00:00:00:00:1", "zz:00:00:00:00:10"] {
            let dictionary = test_result_dictionary(Some(malformed), Some(1500), Some(2048));
            assert_parameter_error(
                super::decode_vmnet_interface_parameters(dictionary.as_ptr()),
                VmnetInterfaceParameterField::MacAddress,
                VmnetInterfaceParameterProblem::Malformed,
            );
        }
    }

    #[test]
    fn vmnet_result_decoder_copies_uuid_and_optional_batch_values() {
        let dictionary = test_result_dictionary(Some("02:00:00:00:00:10"), Some(1500), Some(2048));
        let interface_id = [0x5a; super::VMNET_INTERFACE_ID_LEN];
        set_dictionary_uuid(
            &dictionary,
            super::vmnet_interface_id_key().expect("interface-ID key should exist"),
            interface_id,
        );
        if let Some(key) = super::optional_vmnet_read_max_packets_key() {
            set_dictionary_uint64(&dictionary, key, 500);
        }
        if let Some(key) = super::optional_vmnet_write_max_packets_key() {
            set_dictionary_uint64(&dictionary, key, 7);
        }

        let parameters = decode_test_parameters(&dictionary, &VmnetInterfaceConfig::shared())
            .expect("valid allocated result should decode");
        assert_eq!(
            parameters.realized_mac(),
            GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 0x10])
        );
        assert_eq!(parameters.effective_mtu(), 1500);
        assert_eq!(parameters.maximum_packet_size(), 2048);
        assert_eq!(parameters.interface_id(), Some(interface_id));
        if super::optional_vmnet_read_max_packets_key().is_some() {
            let packet_buffer_size = if parameters.direct_virtio_header_enabled() {
                2048 + usize::try_from(VIRTIO_NET_TX_HEADER_SIZE)
                    .expect("virtio header size should fit usize")
            } else {
                2048
            };
            let expected = (super::VMNET_MAX_BYTES_PER_OPERATION / packet_buffer_size)
                .min(super::VMNET_MAX_PACKETS_PER_OPERATION)
                .min(usize::from(VIRTIO_NET_QUEUE_SIZE)) as u16;
            assert_eq!(parameters.read_max_packets(), Some(expected));
        } else {
            assert_eq!(parameters.read_max_packets(), None);
        }
        if super::optional_vmnet_write_max_packets_key().is_some() {
            assert_eq!(parameters.write_max_packets(), Some(7));
        } else {
            assert_eq!(parameters.write_max_packets(), None);
        }
        assert_eq!(
            parameters.direct_virtio_header_available(),
            super::optional_vmnet_enable_virtio_header_key().is_some()
        );
        assert_eq!(
            parameters.direct_virtio_header_enabled(),
            super::optional_vmnet_enable_virtio_header_key().is_some()
        );

        let debug = format!("{parameters:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("02:00:00:00:00:10"));
        assert!(!debug.contains("2048"));
        assert!(!debug.contains("128"));
    }

    #[test]
    fn vmnet_result_decoder_rejects_wrong_type_and_nil_uuid() {
        let wrong_type = test_result_dictionary(Some("02:00:00:00:00:10"), Some(1500), Some(2048));
        set_dictionary_uint64(
            &wrong_type,
            super::vmnet_interface_id_key().expect("interface-ID key should exist"),
            1,
        );
        assert_parameter_error(
            super::decode_vmnet_interface_parameters(wrong_type.as_ptr()),
            VmnetInterfaceParameterField::InterfaceId,
            VmnetInterfaceParameterProblem::WrongType,
        );

        let nil_uuid = test_result_dictionary(Some("02:00:00:00:00:10"), Some(1500), Some(2048));
        set_dictionary_uuid(
            &nil_uuid,
            super::vmnet_interface_id_key().expect("interface-ID key should exist"),
            [0; super::VMNET_INTERFACE_ID_LEN],
        );
        assert_parameter_error(
            super::decode_vmnet_interface_parameters(nil_uuid.as_ptr()),
            VmnetInterfaceParameterField::InterfaceId,
            VmnetInterfaceParameterProblem::OutOfRange,
        );
    }

    #[test]
    fn vmnet_result_validation_preserves_configured_mac_and_checks_allocated_mac() {
        let configured_mac = GuestMacAddress::from_bytes([0x01, 0, 0, 0, 0, 0x41]);
        let configured = VmnetInterfaceConfig::shared().with_guest_mac(Some(configured_mac));
        let configured_policy = VmnetInterfaceDescriptor::new(&configured)
            .expect("configured descriptor should build")
            .result_policy;
        let parameters = raw_test_parameters(None, 1500, 2048)
            .validate(configured_policy)
            .expect("configured multicast MAC should retain syntax-only API behavior");
        assert_eq!(parameters.realized_mac(), configured_mac);

        let mismatched = GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 0x42]);
        assert_parameter_error(
            raw_test_parameters(Some(mismatched), 1500, 2048).validate(configured_policy),
            VmnetInterfaceParameterField::MacAddress,
            VmnetInterfaceParameterProblem::ConflictsWithRequest,
        );

        let allocated_policy = VmnetInterfaceDescriptor::new(&VmnetInterfaceConfig::host())
            .expect("allocated descriptor should build")
            .result_policy;
        assert_parameter_error(
            raw_test_parameters(None, 1500, 2048).validate(allocated_policy),
            VmnetInterfaceParameterField::MacAddress,
            VmnetInterfaceParameterProblem::Missing,
        );
        for invalid in [
            GuestMacAddress::from_bytes([0; 6]),
            GuestMacAddress::from_bytes([0x01, 0, 0, 0, 0, 1]),
        ] {
            assert_parameter_error(
                raw_test_parameters(Some(invalid), 1500, 2048).validate(allocated_policy),
                VmnetInterfaceParameterField::MacAddress,
                VmnetInterfaceParameterProblem::OutOfRange,
            );
        }
    }

    #[test]
    fn vmnet_result_validation_enforces_mode_specific_mtu_contract() {
        let mac = Some(GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 0x51]));
        for mode in [VmnetInterfaceConfig::host(), VmnetInterfaceConfig::shared()] {
            let policy = VmnetInterfaceDescriptor::new(&mode.with_mtu(Some(1400)))
                .expect("requested-MTU descriptor should build")
                .result_policy;
            assert_parameter_error(
                raw_test_parameters(mac, 1500, 2048).validate(policy),
                VmnetInterfaceParameterField::EffectiveMtu,
                VmnetInterfaceParameterProblem::ConflictsWithRequest,
            );
        }

        let bridged = VmnetInterfaceConfig::bridged("en0")
            .expect("bridged config should build")
            .with_mtu(Some(1500));
        let bridged_policy = VmnetInterfaceDescriptor::new(&bridged)
            .expect("bridged descriptor should build")
            .result_policy;
        assert_parameter_error(
            raw_test_parameters(mac, 1400, 2048).validate(bridged_policy),
            VmnetInterfaceParameterField::EffectiveMtu,
            VmnetInterfaceParameterProblem::ConflictsWithRequest,
        );
        assert!(
            raw_test_parameters(mac, 1600, 2048)
                .validate(bridged_policy)
                .is_ok()
        );

        let unrequested_policy = VmnetInterfaceDescriptor::new(&VmnetInterfaceConfig::host())
            .expect("host descriptor should build")
            .result_policy;
        for invalid in [
            u64::from(VIRTIO_NET_MIN_MTU) - 1,
            u64::from(VIRTIO_NET_MAX_MTU) + 1,
            u64::MAX,
        ] {
            assert_parameter_error(
                raw_test_parameters(mac, invalid, 2048).validate(unrequested_policy),
                VmnetInterfaceParameterField::EffectiveMtu,
                VmnetInterfaceParameterProblem::OutOfRange,
            );
        }
    }

    #[test]
    fn vmnet_result_validation_bounds_packet_and_direct_header_sizes() {
        let mac = Some(GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 0x61]));
        let base_policy = VmnetInterfaceDescriptor::new(&VmnetInterfaceConfig::host())
            .expect("host descriptor should build")
            .result_policy;
        let raw_policy = super::VmnetInterfaceResultPolicy {
            direct_virtio_header_enabled: false,
            ..base_policy
        };
        for invalid in [
            0,
            u64::from(VIRTIO_NET_MIN_MTU) - 1,
            VIRTIO_NET_MAX_BUFFER_SIZE,
            VIRTIO_NET_MAX_BUFFER_SIZE + 1,
            u64::MAX,
        ] {
            assert_parameter_error(
                raw_test_parameters(mac, u64::from(VIRTIO_NET_MIN_MTU), invalid)
                    .validate(raw_policy),
                VmnetInterfaceParameterField::MaximumPacketSize,
                VmnetInterfaceParameterProblem::OutOfRange,
            );
        }
        assert!(
            raw_test_parameters(
                mac,
                1500,
                VIRTIO_NET_MAX_BUFFER_SIZE - u64::from(VIRTIO_NET_TX_HEADER_SIZE),
            )
            .validate(raw_policy)
            .is_ok()
        );

        let unavailable_direct = super::VmnetInterfaceResultPolicy {
            direct_virtio_header_available: false,
            direct_virtio_header_enabled: true,
            ..base_policy
        };
        assert_parameter_error(
            raw_test_parameters(mac, 1500, 2048).validate(unavailable_direct),
            VmnetInterfaceParameterField::DirectVirtioHeader,
            VmnetInterfaceParameterProblem::ConflictsWithRequest,
        );

        let enabled_direct = super::VmnetInterfaceResultPolicy {
            direct_virtio_header_available: true,
            direct_virtio_header_enabled: true,
            ..base_policy
        };
        assert_parameter_error(
            raw_test_parameters(mac, 1500, VIRTIO_NET_MAX_BUFFER_SIZE).validate(enabled_direct),
            VmnetInterfaceParameterField::DirectVirtioHeader,
            VmnetInterfaceParameterProblem::OutOfRange,
        );
        assert!(
            raw_test_parameters(
                mac,
                1500,
                VIRTIO_NET_MAX_BUFFER_SIZE - u64::from(VIRTIO_NET_TX_HEADER_SIZE),
            )
            .validate(enabled_direct)
            .is_ok()
        );
    }

    #[test]
    fn vmnet_result_validation_rejects_zero_and_overflowing_batches_and_caps_valid_values() {
        let mac = Some(GuestMacAddress::from_bytes([0x02, 0, 0, 0, 0, 0x71]));
        let policy = VmnetInterfaceDescriptor::new(&VmnetInterfaceConfig::host())
            .expect("host descriptor should build")
            .result_policy;
        for (field, read, write) in [
            (
                VmnetInterfaceParameterField::ReadMaximumPackets,
                Some(0),
                None,
            ),
            (
                VmnetInterfaceParameterField::WriteMaximumPackets,
                None,
                Some(c_int::MAX as u64 + 1),
            ),
        ] {
            let mut raw = raw_test_parameters(mac, 1500, 2048);
            raw.read_max_packets = read;
            raw.write_max_packets = write;
            assert_parameter_error(
                raw.validate(policy),
                field,
                VmnetInterfaceParameterProblem::OutOfRange,
            );
        }

        let mut raw = raw_test_parameters(mac, 1500, 2048);
        raw.read_max_packets = Some(500);
        raw.write_max_packets = Some(500);
        let parameters = raw
            .validate(policy)
            .expect("valid batches should be capped");
        let packet_buffer_size = if policy.direct_virtio_header_enabled {
            2048 + usize::try_from(VIRTIO_NET_TX_HEADER_SIZE)
                .expect("virtio header size should fit usize")
        } else {
            2048
        };
        let expected = (super::VMNET_MAX_BYTES_PER_OPERATION / packet_buffer_size)
            .min(super::VMNET_MAX_PACKETS_PER_OPERATION)
            .min(usize::from(VIRTIO_NET_QUEUE_SIZE)) as u16;
        assert_eq!(parameters.read_max_packets(), Some(expected));
        assert_eq!(parameters.write_max_packets(), Some(expected));
    }

    #[test]
    fn owned_interface_starts_and_stops_once() {
        let lifecycle = RecordingVmnetLifecycle::new();
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::host();
        let mut interface = OwnedVmnetInterface::start(lifecycle, &config)
            .expect("owned vmnet interface should start");

        assert!(interface.is_started());
        interface.stop().expect("owned vmnet interface should stop");
        assert!(!interface.is_started());
        interface.stop().expect("second stop should be a no-op");

        assert_eq!(recorded_events(&event_log), ["start:host", "stop:7"]);
    }

    #[test]
    fn owned_interface_drop_stops_started_interface() {
        let lifecycle = RecordingVmnetLifecycle::new();
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::shared();

        {
            let _interface = OwnedVmnetInterface::start(lifecycle, &config)
                .expect("owned vmnet interface should start");
        }

        assert_eq!(recorded_events(&event_log), ["start:shared", "stop:7"]);
    }

    #[test]
    fn owned_interface_start_failure_does_not_create_owner() {
        let lifecycle =
            RecordingVmnetLifecycle::new().with_start_status(VmnetStatus::InvalidAccess);
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::host();
        let error = OwnedVmnetInterface::start(lifecycle, &config)
            .expect_err("start failure should return an error");

        assert_eq!(error.operation(), VmnetOperation::StartInterface);
        assert_eq!(error.status(), VmnetStatus::InvalidAccess);
        assert_eq!(recorded_events(&event_log), ["start:host"]);
    }

    #[test]
    fn owned_interface_failed_stop_marks_owner_uncertain_without_retry() {
        let lifecycle =
            RecordingVmnetLifecycle::new().with_stop_status(VmnetStatus::SetupIncomplete);
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::host();
        let mut interface = OwnedVmnetInterface::start(lifecycle, &config)
            .expect("owned vmnet interface should start");
        let error = interface
            .stop()
            .expect_err("failed stop should return an error");

        assert_eq!(error.operation(), VmnetOperation::StopInterface);
        assert_eq!(error.status(), VmnetStatus::SetupIncomplete);
        assert!(!interface.is_started());
        assert!(interface.is_uncertain());
        interface
            .stop()
            .expect_err("uncertain owner must not retry stop");
        assert_eq!(recorded_events(&event_log), ["start:host", "stop:7"]);
    }

    #[test]
    fn owned_interface_drop_does_not_retry_after_failed_explicit_stop() {
        let lifecycle =
            RecordingVmnetLifecycle::new().with_stop_status(VmnetStatus::SetupIncomplete);
        let event_log = lifecycle.events();
        let config = VmnetInterfaceConfig::host();

        {
            let mut interface = OwnedVmnetInterface::start(lifecycle, &config)
                .expect("owned vmnet interface should start");

            let _ = interface.stop();
        }

        assert_eq!(recorded_events(&event_log), ["start:host", "stop:7"]);
    }

    #[test]
    fn vmnet_write_operation_displays_name() {
        let error = VmnetError::new(VmnetOperation::WritePackets, VmnetStatus::PacketTooBig);

        assert_eq!(
            error.to_string(),
            "vmnet_write failed with VMNET_PACKET_TOO_BIG"
        );
    }
}
