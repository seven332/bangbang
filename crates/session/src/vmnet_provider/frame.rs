use std::fmt;
use std::os::unix::net::UnixStream;

use crate::SessionId;

use super::VmnetProviderError;

/// Encoded provider-v1 header size.
pub const HEADER_BYTES: usize = 80;
/// Control-channel discriminant.
pub const CONTROL_CHANNEL: u8 = 1;
/// Per-interface data-channel discriminant.
pub const DATA_CHANNEL: u8 = 2;
/// Tight production authority limit for active system-vmnet interfaces.
pub const MAX_ACTIVE_INTERFACES: usize = crate::MAX_VMNET_ACTIVE_INTERFACES as usize;
/// Existing local vmnet packet-count bound for one operation.
pub const MAX_PACKET_COUNT: usize = 200;
/// Existing local vmnet aggregate packet-byte bound for one operation.
pub const MAX_AGGREGATE_PACKET_BYTES: usize = 256 * 1024;
/// Largest local packet buffer including an enabled 12-byte virtio header.
pub const MAX_PACKET_BUFFER_BYTES: usize = 65_562 + 12;
const PACKET_BATCH_METADATA_BYTES: usize = 8 + MAX_PACKET_COUNT * size_of_u32();
const MAX_BODY_BYTES: usize = 8 + PACKET_BATCH_METADATA_BYTES + MAX_AGGREGATE_PACKET_BYTES;
/// Largest complete encoded provider frame.
pub const MAX_FRAME_BYTES: usize = HEADER_BYTES + MAX_BODY_BYTES;
/// Incremental decoder buffering limit.
pub const MAX_BUFFERED_FRAME_BYTES: usize = MAX_FRAME_BYTES * 2;

const MAGIC: [u8; 8] = *b"BBVNETP\0";
const VERSION: u16 = 1;
const VIRTIO_HEADER_BYTES: usize = 12;
const MIN_MTU: u16 = 68;
const MAX_VMNET_PACKET_BYTES: usize = 65_562;
const BACKEND_INTERFACE_ID_BYTES: usize = 16;

const CONTROL_HELLO: u8 = 1;
const CONTROL_HELLO_ACK: u8 = 2;
const CONTROL_START: u8 = 3;
const CONTROL_STARTED: u8 = 4;
const CONTROL_START_FAILED: u8 = 5;
const CONTROL_STOP: u8 = 6;
const CONTROL_STOPPED: u8 = 7;
const CONTROL_CANCEL: u8 = 8;
const CONTROL_CANCELLED: u8 = 9;
const CONTROL_SHUTDOWN: u8 = 10;
const CONTROL_SHUTDOWN_ACK: u8 = 11;
const CONTROL_TERMINAL: u8 = 12;

const DATA_HELLO: u8 = 32;
const DATA_HELLO_ACK: u8 = 33;
const DATA_READINESS: u8 = 34;
const DATA_READ: u8 = 35;
const DATA_READ_RESULT: u8 = 36;
const DATA_WRITE: u8 = 37;
const DATA_WRITE_RESULT: u8 = 38;
const DATA_OPERATION_FAILED: u8 = 39;
const DATA_STOP: u8 = 40;
const DATA_STOPPED: u8 = 41;
const DATA_SHUTDOWN: u8 = 42;
const DATA_SHUTDOWN_ACK: u8 = 43;
const DATA_TERMINAL: u8 = 44;

const REQUEST_MAC_PRESENT: u8 = 1 << 0;
const REQUEST_MTU_PRESENT: u8 = 1 << 1;
const REQUEST_FLAGS: u8 = REQUEST_MAC_PRESENT | REQUEST_MTU_PRESENT;

const REALIZED_BACKEND_ID_PRESENT: u16 = 1 << 0;
const REALIZED_READ_MAX_PRESENT: u16 = 1 << 1;
const REALIZED_WRITE_MAX_PRESENT: u16 = 1 << 2;
const REALIZED_DIRECT_AVAILABLE: u16 = 1 << 3;
const REALIZED_DIRECT_ENABLED: u16 = 1 << 4;
const REALIZED_FLAGS: u16 = REALIZED_BACKEND_ID_PRESENT
    | REALIZED_READ_MAX_PRESENT
    | REALIZED_WRITE_MAX_PRESENT
    | REALIZED_DIRECT_AVAILABLE
    | REALIZED_DIRECT_ENABLED;

const fn size_of_u32() -> usize {
    4
}

macro_rules! nonzero_identifier {
    ($name:ident, $inner:ty, $label:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name($inner);

        impl $name {
            /// Smallest valid wire identity.
            pub const MIN: Self = Self(1);

            /// Constructs a validated nonzero identity.
            pub const fn new(value: $inner) -> Result<Self, VmnetProviderError> {
                if value == 0 {
                    Err(VmnetProviderError::InvalidConfiguration)
                } else {
                    Ok(Self(value))
                }
            }

            /// Returns the exact numeric wire value.
            #[must_use]
            pub const fn get(self) -> $inner {
                self.0
            }

            pub(crate) const fn decode(value: $inner) -> Result<Self, VmnetProviderError> {
                if value == 0 {
                    Err(VmnetProviderError::InvalidFrame)
                } else {
                    Ok(Self(value))
                }
            }

            /// Returns the next nonzero value without wrapping.
            pub const fn checked_next(self) -> Result<Self, VmnetProviderError> {
                match self.0.checked_add(1) {
                    Some(next) => Ok(Self(next)),
                    None => Err(VmnetProviderError::LimitExceeded),
                }
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(concat!($label, "(<redacted>)"))
            }
        }
    };
}

nonzero_identifier!(VmnetInterfaceId, u32, "VmnetInterfaceId");
nonzero_identifier!(VmnetGeneration, u64, "VmnetGeneration");
nonzero_identifier!(VmnetSequence, u64, "VmnetSequence");
nonzero_identifier!(VmnetReadinessEpoch, u64, "VmnetReadinessEpoch");

/// Fixed bootstrap-owned vmnet policy slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VmnetPolicySlot {
    /// Host-mode authority.
    Host = 1,
    /// Shared-mode authority.
    Shared = 2,
    /// First bootstrap-owned bridged selector.
    Bridge0 = 3,
    /// Second bootstrap-owned bridged selector.
    Bridge1 = 4,
    /// Third bootstrap-owned bridged selector.
    Bridge2 = 5,
    /// Fourth bootstrap-owned bridged selector.
    Bridge3 = 6,
}

impl VmnetPolicySlot {
    fn decode(value: u8) -> Result<Self, VmnetProviderError> {
        match value {
            1 => Ok(Self::Host),
            2 => Ok(Self::Shared),
            3 => Ok(Self::Bridge0),
            4 => Ok(Self::Bridge1),
            5 => Ok(Self::Bridge2),
            6 => Ok(Self::Bridge3),
            _ => Err(VmnetProviderError::InvalidFrame),
        }
    }
}

/// Optional typed parameters requested for one policy-slot start.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RequestedVmnetParameters {
    mac: Option<[u8; 6]>,
    mtu: Option<u16>,
}

impl RequestedVmnetParameters {
    /// Constructs one canonical optional request.
    pub fn new(mac: Option<[u8; 6]>, mtu: Option<u16>) -> Result<Self, VmnetProviderError> {
        if mac.is_some_and(|mac| !valid_unicast_mac(mac)) || mtu.is_some_and(|mtu| mtu < MIN_MTU) {
            return Err(VmnetProviderError::InvalidConfiguration);
        }
        Ok(Self { mac, mtu })
    }

    /// Returns the optional requested guest MAC.
    #[must_use]
    pub const fn mac(self) -> Option<[u8; 6]> {
        self.mac
    }

    /// Returns the optional requested MTU.
    #[must_use]
    pub const fn mtu(self) -> Option<u16> {
        self.mtu
    }
}

impl fmt::Debug for RequestedVmnetParameters {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RequestedVmnetParameters(<redacted>)")
    }
}

/// Validated realized interface parameters returned by a provider owner.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RealizedVmnetParameters {
    mac: [u8; 6],
    effective_mtu: u16,
    maximum_packet_bytes: u32,
    backend_interface_id: Option<[u8; BACKEND_INTERFACE_ID_BYTES]>,
    read_max_packets: Option<u16>,
    write_max_packets: Option<u16>,
    direct_virtio_header_available: bool,
    direct_virtio_header_enabled: bool,
}

impl RealizedVmnetParameters {
    /// Constructs one bounded canonical realized parameter set.
    pub fn new(
        mac: [u8; 6],
        effective_mtu: u16,
        maximum_packet_bytes: u32,
    ) -> Result<Self, VmnetProviderError> {
        Self {
            mac,
            effective_mtu,
            maximum_packet_bytes,
            backend_interface_id: None,
            read_max_packets: None,
            write_max_packets: None,
            direct_virtio_header_available: false,
            direct_virtio_header_enabled: false,
        }
        .validated()
    }

    /// Adds one optional opaque backend identity.
    pub fn with_backend_interface_id(
        mut self,
        backend_interface_id: Option<[u8; BACKEND_INTERFACE_ID_BYTES]>,
    ) -> Result<Self, VmnetProviderError> {
        self.backend_interface_id = backend_interface_id;
        self.validated()
    }

    /// Narrows the optional backend read and write batch maxima.
    pub fn with_batch_limits(
        mut self,
        read_max_packets: Option<u16>,
        write_max_packets: Option<u16>,
    ) -> Result<Self, VmnetProviderError> {
        self.read_max_packets = read_max_packets;
        self.write_max_packets = write_max_packets;
        self.validated()
    }

    /// Declares coherent direct-virtio-header availability and selection.
    pub fn with_direct_virtio_header(
        mut self,
        direct_virtio_header_available: bool,
        direct_virtio_header_enabled: bool,
    ) -> Result<Self, VmnetProviderError> {
        self.direct_virtio_header_available = direct_virtio_header_available;
        self.direct_virtio_header_enabled = direct_virtio_header_enabled;
        self.validated()
    }

    fn validated(self) -> Result<Self, VmnetProviderError> {
        let maximum = usize::try_from(self.maximum_packet_bytes)
            .map_err(|_| VmnetProviderError::InvalidConfiguration)?;
        let packet_buffer = maximum
            .checked_add(if self.direct_virtio_header_enabled {
                VIRTIO_HEADER_BYTES
            } else {
                0
            })
            .ok_or(VmnetProviderError::InvalidConfiguration)?;
        if !valid_unicast_mac(self.mac)
            || self.effective_mtu < MIN_MTU
            || maximum == 0
            || maximum > MAX_VMNET_PACKET_BYTES
            || packet_buffer > MAX_PACKET_BUFFER_BYTES
            || self.direct_virtio_header_enabled && !self.direct_virtio_header_available
            || self
                .backend_interface_id
                .is_some_and(|identity| identity == [0; 16])
        {
            return Err(VmnetProviderError::InvalidConfiguration);
        }
        let derived_max = MAX_PACKET_COUNT.min(MAX_AGGREGATE_PACKET_BYTES / packet_buffer);
        if derived_max == 0
            || self
                .read_max_packets
                .is_some_and(|value| value == 0 || usize::from(value) > derived_max)
            || self
                .write_max_packets
                .is_some_and(|value| value == 0 || usize::from(value) > derived_max)
        {
            return Err(VmnetProviderError::InvalidConfiguration);
        }
        Ok(self)
    }

    /// Returns the realized guest MAC.
    #[must_use]
    pub const fn mac(self) -> [u8; 6] {
        self.mac
    }

    /// Returns the realized MTU.
    #[must_use]
    pub const fn effective_mtu(self) -> u16 {
        self.effective_mtu
    }

    /// Returns the backend packet size excluding an optional direct header.
    #[must_use]
    pub const fn maximum_packet_bytes(self) -> u32 {
        self.maximum_packet_bytes
    }

    /// Returns the optional opaque backend interface identity.
    #[must_use]
    pub const fn backend_interface_id(self) -> Option<[u8; BACKEND_INTERFACE_ID_BYTES]> {
        self.backend_interface_id
    }

    /// Returns the optional backend read maximum.
    #[must_use]
    pub const fn read_max_packets(self) -> Option<u16> {
        self.read_max_packets
    }

    /// Returns the optional backend write maximum.
    #[must_use]
    pub const fn write_max_packets(self) -> Option<u16> {
        self.write_max_packets
    }

    /// Returns whether direct virtio headers are available.
    #[must_use]
    pub const fn direct_virtio_header_available(self) -> bool {
        self.direct_virtio_header_available
    }

    /// Returns whether direct virtio headers are enabled.
    #[must_use]
    pub const fn direct_virtio_header_enabled(self) -> bool {
        self.direct_virtio_header_enabled
    }

    /// Returns the exact provider packet-buffer size.
    #[must_use]
    pub fn packet_buffer_bytes(self) -> usize {
        usize::try_from(self.maximum_packet_bytes)
            .unwrap_or(0)
            .saturating_add(if self.direct_virtio_header_enabled {
                VIRTIO_HEADER_BYTES
            } else {
                0
            })
    }

    /// Returns the effective read batch maximum.
    #[must_use]
    pub fn effective_read_max_packets(self) -> u16 {
        self.read_max_packets
            .unwrap_or_else(|| self.derived_batch_max())
    }

    /// Returns the effective write batch maximum.
    #[must_use]
    pub fn effective_write_max_packets(self) -> u16 {
        self.write_max_packets
            .unwrap_or_else(|| self.derived_batch_max())
    }

    fn derived_batch_max(self) -> u16 {
        u16::try_from(
            MAX_PACKET_COUNT.min(MAX_AGGREGATE_PACKET_BYTES / self.packet_buffer_bytes().max(1)),
        )
        .unwrap_or(1)
    }
}

impl fmt::Debug for RealizedVmnetParameters {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("RealizedVmnetParameters(<redacted>)")
    }
}

fn valid_unicast_mac(mac: [u8; 6]) -> bool {
    mac != [0; 6] && mac != [0xff; 6] && mac.first().is_some_and(|byte| byte & 1 == 0)
}

/// Closed provider failure category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum ProviderStatus {
    /// Policy slot is not authorized.
    PolicyDenied = 1,
    /// Active-interface capacity is exhausted.
    ResourceLimit = 2,
    /// Platform authorization rejected the operation.
    NotAuthorized = 3,
    /// Shared networking service is busy.
    SharingServiceBusy = 4,
    /// Typed request was rejected by the backend.
    InvalidArgument = 5,
    /// Backend memory allocation failed.
    MemoryFailure = 6,
    /// A packet exceeded the backend limit.
    PacketTooBig = 7,
    /// Backend buffering is exhausted.
    BufferExhausted = 8,
    /// Backend packet count is too large.
    TooManyPackets = 9,
    /// Start did not complete.
    SetupIncomplete = 10,
    /// A bounded backend operation failed.
    BackendFailure = 11,
    /// Cleanup ownership cannot be proved.
    CleanupUncertain = 12,
}

impl ProviderStatus {
    fn decode(value: u16) -> Result<Self, VmnetProviderError> {
        match value {
            1 => Ok(Self::PolicyDenied),
            2 => Ok(Self::ResourceLimit),
            3 => Ok(Self::NotAuthorized),
            4 => Ok(Self::SharingServiceBusy),
            5 => Ok(Self::InvalidArgument),
            6 => Ok(Self::MemoryFailure),
            7 => Ok(Self::PacketTooBig),
            8 => Ok(Self::BufferExhausted),
            9 => Ok(Self::TooManyPackets),
            10 => Ok(Self::SetupIncomplete),
            11 => Ok(Self::BackendFailure),
            12 => Ok(Self::CleanupUncertain),
            _ => Err(VmnetProviderError::InvalidFrame),
        }
    }
}

/// Cleanup certainty returned by stop or cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProviderCleanup {
    /// Every owned interface/resource was retired.
    Complete = 1,
    /// Cleanup could not be confirmed.
    Uncertain = 2,
}

impl ProviderCleanup {
    fn decode(value: u8) -> Result<Self, VmnetProviderError> {
        match value {
            1 => Ok(Self::Complete),
            2 => Ok(Self::Uncertain),
            _ => Err(VmnetProviderError::InvalidFrame),
        }
    }
}

/// Session cancellation category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProviderCancelReason {
    /// The worker no longer needs the session.
    Worker = 1,
    /// The launcher is terminating the session.
    Launcher = 2,
}

impl ProviderCancelReason {
    fn decode(value: u8) -> Result<Self, VmnetProviderError> {
        match value {
            1 => Ok(Self::Worker),
            2 => Ok(Self::Launcher),
            _ => Err(VmnetProviderError::InvalidFrame),
        }
    }
}

/// Redacted terminal protocol category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProviderTerminalCode {
    /// Peer input violated the protocol.
    Protocol = 1,
    /// A local backend operation failed.
    Backend = 2,
    /// Cleanup ownership became uncertain.
    Cleanup = 3,
    /// Supervising process requested termination.
    Supervisor = 4,
}

impl ProviderTerminalCode {
    fn decode(value: u8) -> Result<Self, VmnetProviderError> {
        match value {
            1 => Ok(Self::Protocol),
            2 => Ok(Self::Backend),
            3 => Ok(Self::Cleanup),
            4 => Ok(Self::Supervisor),
            _ => Err(VmnetProviderError::InvalidFrame),
        }
    }
}

/// Per-interface backend operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ProviderOperation {
    /// Read one bounded packet batch.
    Read = 1,
    /// Write one bounded packet batch.
    Write = 2,
}

impl ProviderOperation {
    fn decode(value: u8) -> Result<Self, VmnetProviderError> {
        match value {
            1 => Ok(Self::Read),
            2 => Ok(Self::Write),
            _ => Err(VmnetProviderError::InvalidFrame),
        }
    }
}

/// Canonical ordered packet batch.
#[derive(Clone, PartialEq, Eq)]
pub struct VmnetPacketBatch {
    lengths: Vec<u32>,
    bytes: Vec<u8>,
}

impl VmnetPacketBatch {
    /// Constructs a canonical read result; the packet list may be empty.
    pub fn read(packets: &[&[u8]]) -> Result<Self, VmnetProviderError> {
        Self::from_packets(packets, true)
    }

    /// Constructs a canonical nonempty write request.
    pub fn write(packets: &[&[u8]]) -> Result<Self, VmnetProviderError> {
        Self::from_packets(packets, false)
    }

    fn from_packets(packets: &[&[u8]], allow_empty: bool) -> Result<Self, VmnetProviderError> {
        if (!allow_empty && packets.is_empty()) || packets.len() > MAX_PACKET_COUNT {
            return Err(VmnetProviderError::InvalidConfiguration);
        }
        let aggregate = packets.iter().try_fold(0_usize, |total, packet| {
            if packet.is_empty() || packet.len() > MAX_PACKET_BUFFER_BYTES {
                return Err(VmnetProviderError::InvalidConfiguration);
            }
            total
                .checked_add(packet.len())
                .filter(|total| *total <= MAX_AGGREGATE_PACKET_BYTES)
                .ok_or(VmnetProviderError::LimitExceeded)
        })?;
        let mut lengths = Vec::new();
        lengths
            .try_reserve_exact(packets.len())
            .map_err(|_| VmnetProviderError::LimitExceeded)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(aggregate)
            .map_err(|_| VmnetProviderError::LimitExceeded)?;
        for packet in packets {
            lengths
                .push(u32::try_from(packet.len()).map_err(|_| VmnetProviderError::LimitExceeded)?);
            bytes.extend_from_slice(packet);
        }
        Ok(Self { lengths, bytes })
    }

    fn from_parts(
        lengths: Vec<u32>,
        bytes: Vec<u8>,
        allow_empty: bool,
    ) -> Result<Self, VmnetProviderError> {
        if (!allow_empty && lengths.is_empty()) || lengths.len() > MAX_PACKET_COUNT {
            return Err(VmnetProviderError::InvalidFrame);
        }
        let total = lengths.iter().try_fold(0_usize, |total, length| {
            let length = usize::try_from(*length).map_err(|_| VmnetProviderError::InvalidFrame)?;
            if length == 0 || length > MAX_PACKET_BUFFER_BYTES {
                return Err(VmnetProviderError::InvalidFrame);
            }
            total
                .checked_add(length)
                .filter(|total| *total <= MAX_AGGREGATE_PACKET_BYTES)
                .ok_or(VmnetProviderError::LimitExceeded)
        })?;
        if total != bytes.len() {
            return Err(VmnetProviderError::InvalidFrame);
        }
        Ok(Self { lengths, bytes })
    }

    /// Returns the packet count.
    #[must_use]
    pub fn packet_count(&self) -> usize {
        self.lengths.len()
    }

    /// Returns the aggregate packet byte count.
    #[must_use]
    pub fn aggregate_bytes(&self) -> usize {
        self.bytes.len()
    }

    /// Borrows one packet by canonical index.
    #[must_use]
    pub fn packet(&self, requested_index: usize) -> Option<&[u8]> {
        let mut offset = 0_usize;
        for (index, length) in self.lengths.iter().copied().enumerate() {
            let length = usize::try_from(length).ok()?;
            let end = offset.checked_add(length)?;
            if index == requested_index {
                return self.bytes.get(offset..end);
            }
            offset = end;
        }
        None
    }

    pub(crate) fn fits(&self, maximum_packet_bytes: usize, maximum_packets: usize) -> bool {
        self.packet_count() <= maximum_packets
            && self.lengths.iter().copied().all(|length| {
                usize::try_from(length).is_ok_and(|length| length <= maximum_packet_bytes)
            })
    }
}

impl fmt::Debug for VmnetPacketBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetPacketBatch")
            .field("packet_count", &self.lengths.len())
            .field("bytes", &"<redacted>")
            .finish()
    }
}

/// One frame and its atomically associated optional transferred stream.
///
/// Constructors are private to the protocol state and transport modules.
/// Application adapters can inspect a frame but cannot detach a stream before
/// the matching state transition accepts it.
pub struct ProviderEnvelope {
    frame: ProviderFrame,
    stream: Option<UnixStream>,
}

impl ProviderEnvelope {
    pub(crate) const fn frame_only(frame: ProviderFrame) -> Self {
        Self {
            frame,
            stream: None,
        }
    }

    pub(crate) const fn with_stream(frame: ProviderFrame, stream: UnixStream) -> Self {
        Self {
            frame,
            stream: Some(stream),
        }
    }

    /// Borrows the decoded frame without exposing descriptor ownership.
    #[must_use]
    pub const fn frame(&self) -> &ProviderFrame {
        &self.frame
    }

    /// Returns whether this envelope owns the sole transferred stream.
    #[must_use]
    pub const fn has_stream(&self) -> bool {
        self.stream.is_some()
    }

    pub(crate) fn into_parts(self) -> (ProviderFrame, Option<UnixStream>) {
        (self.frame, self.stream)
    }
}

impl fmt::Debug for ProviderEnvelope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProviderEnvelope")
            .field("frame", &self.frame)
            .field("stream", &self.stream.as_ref().map(|_| "<owned>"))
            .finish()
    }
}

/// Closed control-channel message set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlMessage {
    /// Client role and session preamble.
    Hello,
    /// Broker acknowledgement.
    HelloAck,
    /// Starts one interface using a fixed policy slot.
    Start {
        /// Bootstrap-owned selector slot.
        policy_slot: VmnetPolicySlot,
        /// Optional typed device request.
        requested: RequestedVmnetParameters,
    },
    /// Returns realized parameters and declares one transferred data stream.
    Started {
        /// Validated realized backend parameters.
        parameters: RealizedVmnetParameters,
    },
    /// Returns a typed start failure with no descriptor.
    StartFailed {
        /// Redacted failure category.
        status: ProviderStatus,
    },
    /// Requests owner stop and process retirement.
    Stop,
    /// Reports broker-side interface retirement.
    Stopped {
        /// Cleanup certainty.
        cleanup: ProviderCleanup,
    },
    /// Cancels every pending and active generation.
    Cancel {
        /// Stable cancellation source.
        reason: ProviderCancelReason,
    },
    /// Acknowledges session-wide cancellation.
    Cancelled {
        /// Aggregate cleanup certainty.
        cleanup: ProviderCleanup,
    },
    /// Requests orderly empty-session shutdown.
    Shutdown,
    /// Acknowledges orderly shutdown.
    ShutdownAck,
    /// Reports a terminal failure.
    Terminal {
        /// Stable terminal category.
        code: ProviderTerminalCode,
    },
}

/// Closed per-interface data-channel message set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataMessage {
    /// Client repeats the exact transferred-stream binding.
    Hello,
    /// Owner acknowledges the exact binding.
    HelloAck,
    /// Owner publishes one bounded readiness edge.
    Readiness {
        /// Contiguous per-interface readiness epoch.
        epoch: VmnetReadinessEpoch,
        /// Bounded backend packet estimate.
        estimated_packets: u16,
    },
    /// Client requests up to a bounded packet count.
    Read {
        /// Nonzero requested packet maximum.
        max_packets: u16,
    },
    /// Owner returns a zero-to-requested packet prefix.
    ReadResult {
        /// Exact client request sequence.
        request: VmnetSequence,
        /// Canonical read batch.
        packets: VmnetPacketBatch,
    },
    /// Client submits one nonempty canonical packet batch.
    Write {
        /// Canonical write batch.
        packets: VmnetPacketBatch,
    },
    /// Owner returns the successfully written prefix length.
    WriteResult {
        /// Exact client request sequence.
        request: VmnetSequence,
        /// Zero-to-requested completed prefix.
        completed_packets: u16,
    },
    /// Owner reports a terminal read/write failure.
    OperationFailed {
        /// Exact client request sequence.
        request: VmnetSequence,
        /// Failed operation.
        operation: ProviderOperation,
        /// Redacted backend category.
        status: ProviderStatus,
    },
    /// Requests callback drain and backend stop.
    Stop,
    /// Reports backend stop certainty.
    Stopped {
        /// Cleanup certainty.
        cleanup: ProviderCleanup,
    },
    /// Requests closure after a complete stop.
    Shutdown,
    /// Acknowledges orderly stream closure.
    ShutdownAck,
    /// Reports a terminal per-interface failure.
    Terminal {
        /// Stable terminal category.
        code: ProviderTerminalCode,
    },
}

/// Provider frame channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderChannel {
    /// Session-wide broker control.
    Control,
    /// One exact interface generation's packet plane.
    Data,
}

impl ProviderChannel {
    const fn byte(self) -> u8 {
        match self {
            Self::Control => CONTROL_CHANNEL,
            Self::Data => DATA_CHANNEL,
        }
    }

    fn decode(value: u8) -> Result<Self, VmnetProviderError> {
        match value {
            CONTROL_CHANNEL => Ok(Self::Control),
            DATA_CHANNEL => Ok(Self::Data),
            _ => Err(VmnetProviderError::InvalidFrame),
        }
    }
}

/// One decoded provider frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderFrame {
    session: SessionId,
    interface: Option<VmnetInterfaceId>,
    generation: Option<VmnetGeneration>,
    sequence: VmnetSequence,
    message: ProviderMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProviderMessage {
    Control(ControlMessage),
    Data(DataMessage),
}

impl ProviderFrame {
    /// Constructs a shape-checked control frame.
    pub fn control(
        session: SessionId,
        interface: Option<VmnetInterfaceId>,
        generation: Option<VmnetGeneration>,
        sequence: VmnetSequence,
        message: ControlMessage,
    ) -> Result<Self, VmnetProviderError> {
        let frame = Self {
            session,
            interface,
            generation,
            sequence,
            message: ProviderMessage::Control(message),
        };
        frame.validate_shape(false)?;
        Ok(frame)
    }

    /// Constructs a shape-checked per-interface data frame.
    pub fn data(
        session: SessionId,
        interface: VmnetInterfaceId,
        generation: VmnetGeneration,
        sequence: VmnetSequence,
        message: DataMessage,
    ) -> Result<Self, VmnetProviderError> {
        let frame = Self {
            session,
            interface: Some(interface),
            generation: Some(generation),
            sequence,
            message: ProviderMessage::Data(message),
        };
        frame.validate_shape(false)?;
        Ok(frame)
    }

    /// Returns the bound lifecycle session.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// Returns the optional interface scope.
    #[must_use]
    pub const fn interface(&self) -> Option<VmnetInterfaceId> {
        self.interface
    }

    /// Returns the optional interface generation.
    #[must_use]
    pub const fn generation(&self) -> Option<VmnetGeneration> {
        self.generation
    }

    /// Returns the exact sender sequence.
    #[must_use]
    pub const fn sequence(&self) -> VmnetSequence {
        self.sequence
    }

    /// Returns the closed channel.
    #[must_use]
    pub const fn channel(&self) -> ProviderChannel {
        match self.message {
            ProviderMessage::Control(_) => ProviderChannel::Control,
            ProviderMessage::Data(_) => ProviderChannel::Data,
        }
    }

    /// Borrows the control message when this is a control frame.
    #[must_use]
    pub const fn control_message(&self) -> Option<&ControlMessage> {
        match &self.message {
            ProviderMessage::Control(message) => Some(message),
            ProviderMessage::Data(_) => None,
        }
    }

    /// Borrows the data message when this is a data frame.
    #[must_use]
    pub const fn data_message(&self) -> Option<&DataMessage> {
        match &self.message {
            ProviderMessage::Control(_) => None,
            ProviderMessage::Data(message) => Some(message),
        }
    }

    /// Returns the exact descriptor count required by this frame.
    #[must_use]
    pub const fn descriptor_count(&self) -> u8 {
        match self.message {
            ProviderMessage::Control(ControlMessage::Started { .. }) => 1,
            _ => 0,
        }
    }

    fn validate_shape(&self, wire: bool) -> Result<(), VmnetProviderError> {
        let invalid = || {
            if wire {
                VmnetProviderError::InvalidFrame
            } else {
                VmnetProviderError::InvalidConfiguration
            }
        };
        if self.session.is_pre_session() {
            return Err(invalid());
        }
        match &self.message {
            ProviderMessage::Control(message) => match message {
                ControlMessage::Start { .. } | ControlMessage::StartFailed { .. } => {
                    if self.interface.is_none() || self.generation.is_some() {
                        return Err(invalid());
                    }
                }
                ControlMessage::Started { .. }
                | ControlMessage::Stop
                | ControlMessage::Stopped { .. } => {
                    if self.interface.is_none() || self.generation.is_none() {
                        return Err(invalid());
                    }
                }
                ControlMessage::Hello
                | ControlMessage::HelloAck
                | ControlMessage::Cancel { .. }
                | ControlMessage::Cancelled { .. }
                | ControlMessage::Shutdown
                | ControlMessage::ShutdownAck
                | ControlMessage::Terminal { .. } => {
                    if self.interface.is_some() || self.generation.is_some() {
                        return Err(invalid());
                    }
                }
            },
            ProviderMessage::Data(message) => {
                if self.interface.is_none() || self.generation.is_none() {
                    return Err(invalid());
                }
                match message {
                    DataMessage::Read { max_packets } => {
                        if *max_packets == 0 || usize::from(*max_packets) > MAX_PACKET_COUNT {
                            return Err(invalid());
                        }
                    }
                    DataMessage::Readiness {
                        estimated_packets, ..
                    } => {
                        if *estimated_packets == 0
                            || usize::from(*estimated_packets) > MAX_PACKET_COUNT
                        {
                            return Err(invalid());
                        }
                    }
                    DataMessage::Write { packets } if packets.packet_count() == 0 => {
                        return Err(invalid());
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }
}

/// Shape-checked provider header decoded before body allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderFrameHeader {
    channel: ProviderChannel,
    kind: u8,
    body_len: usize,
    descriptor_count: u8,
    session: SessionId,
    interface: Option<VmnetInterfaceId>,
    generation: Option<VmnetGeneration>,
    sequence: VmnetSequence,
}

impl ProviderFrameHeader {
    /// Returns the bounded body length.
    #[must_use]
    pub const fn body_len(self) -> usize {
        self.body_len
    }

    /// Returns the declared descriptor count.
    #[must_use]
    pub const fn descriptor_count(self) -> u8 {
        self.descriptor_count
    }

    /// Returns the frame channel.
    #[must_use]
    pub const fn channel(self) -> ProviderChannel {
        self.channel
    }
}

/// Encodes one complete canonical provider frame.
pub fn encode_frame(frame: &ProviderFrame) -> Result<Vec<u8>, VmnetProviderError> {
    frame.validate_shape(false)?;
    let (kind, body) = encode_message(&frame.message)?;
    if body.len() > MAX_BODY_BYTES {
        return Err(VmnetProviderError::LimitExceeded);
    }
    let body_len = u32::try_from(body.len()).map_err(|_| VmnetProviderError::LimitExceeded)?;
    let mut encoded = Vec::new();
    encoded
        .try_reserve_exact(HEADER_BYTES + body.len())
        .map_err(|_| VmnetProviderError::LimitExceeded)?;
    encoded.extend_from_slice(&MAGIC);
    encoded.extend_from_slice(&VERSION.to_be_bytes());
    encoded.push(frame.channel().byte());
    encoded.push(kind);
    encoded.extend_from_slice(&body_len.to_be_bytes());
    encoded.push(frame.descriptor_count());
    encoded.extend_from_slice(&[0; 7]);
    encoded.extend_from_slice(frame.session.as_bytes());
    encoded.extend_from_slice(
        &frame
            .interface
            .map_or(0, VmnetInterfaceId::get)
            .to_be_bytes(),
    );
    encoded.extend_from_slice(&[0; 4]);
    encoded.extend_from_slice(
        &frame
            .generation
            .map_or(0, VmnetGeneration::get)
            .to_be_bytes(),
    );
    encoded.extend_from_slice(&frame.sequence.get().to_be_bytes());
    encoded.extend_from_slice(&body);
    Ok(encoded)
}

/// Decodes and validates one exact provider header.
pub fn decode_header(encoded: &[u8]) -> Result<ProviderFrameHeader, VmnetProviderError> {
    if encoded.len() != HEADER_BYTES
        || encoded.get(0..8) != Some(MAGIC.as_slice())
        || read_u16(encoded, 8)? != VERSION
        || encoded
            .get(17..24)
            .is_none_or(|reserved| reserved != [0; 7])
        || encoded
            .get(60..64)
            .is_none_or(|reserved| reserved != [0; 4])
    {
        return Err(VmnetProviderError::InvalidFrame);
    }
    let channel = ProviderChannel::decode(read_u8(encoded, 10)?)?;
    let kind = read_u8(encoded, 11)?;
    let body_len =
        usize::try_from(read_u32(encoded, 12)?).map_err(|_| VmnetProviderError::InvalidFrame)?;
    let descriptor_count = read_u8(encoded, 16)?;
    let session = SessionId::from_bytes(read_array(encoded, 24)?);
    if session.is_pre_session() {
        return Err(VmnetProviderError::InvalidFrame);
    }
    if body_len > MAX_BODY_BYTES {
        return Err(VmnetProviderError::LimitExceeded);
    }
    let raw_interface = read_u32(encoded, 56)?;
    let raw_generation = read_u64(encoded, 64)?;
    let interface = (raw_interface != 0)
        .then(|| VmnetInterfaceId::decode(raw_interface))
        .transpose()?;
    let generation = (raw_generation != 0)
        .then(|| VmnetGeneration::decode(raw_generation))
        .transpose()?;
    let sequence = VmnetSequence::decode(read_u64(encoded, 72)?)?;
    validate_header_shape(
        channel,
        kind,
        body_len,
        descriptor_count,
        interface,
        generation,
    )?;
    Ok(ProviderFrameHeader {
        channel,
        kind,
        body_len,
        descriptor_count,
        session,
        interface,
        generation,
        sequence,
    })
}

/// Decodes one exact complete canonical provider frame.
pub fn decode_frame(encoded: &[u8]) -> Result<ProviderFrame, VmnetProviderError> {
    if encoded.len() < HEADER_BYTES {
        return Err(VmnetProviderError::InvalidFrame);
    }
    let header_bytes = encoded
        .get(..HEADER_BYTES)
        .ok_or(VmnetProviderError::InvalidFrame)?;
    let header = decode_header(header_bytes)?;
    let total = HEADER_BYTES
        .checked_add(header.body_len)
        .ok_or(VmnetProviderError::LimitExceeded)?;
    if encoded.len() != total {
        return Err(VmnetProviderError::InvalidFrame);
    }
    let body = encoded
        .get(HEADER_BYTES..total)
        .ok_or(VmnetProviderError::InvalidFrame)?;
    let message = decode_message(header.channel, header.kind, body)?;
    let frame = ProviderFrame {
        session: header.session,
        interface: header.interface,
        generation: header.generation,
        sequence: header.sequence,
        message,
    };
    frame.validate_shape(true)?;
    if frame.descriptor_count() != header.descriptor_count {
        return Err(VmnetProviderError::InvalidFrame);
    }
    Ok(frame)
}

fn validate_header_shape(
    channel: ProviderChannel,
    kind: u8,
    body_len: usize,
    descriptor_count: u8,
    interface: Option<VmnetInterfaceId>,
    generation: Option<VmnetGeneration>,
) -> Result<(), VmnetProviderError> {
    let (expected_channel, scope, expected_descriptors, minimum, maximum) = kind_shape(kind)?;
    if channel != expected_channel
        || descriptor_count != expected_descriptors
        || body_len < minimum
        || body_len > maximum
    {
        return Err(VmnetProviderError::InvalidFrame);
    }
    let scope_valid = match scope {
        0 => interface.is_none() && generation.is_none(),
        1 => interface.is_some() && generation.is_none(),
        2 => interface.is_some() && generation.is_some(),
        _ => false,
    };
    scope_valid
        .then_some(())
        .ok_or(VmnetProviderError::InvalidFrame)
}

fn kind_shape(kind: u8) -> Result<(ProviderChannel, u8, u8, usize, usize), VmnetProviderError> {
    let fixed = |channel, scope, descriptors, bytes| (channel, scope, descriptors, bytes, bytes);
    Ok(match kind {
        CONTROL_HELLO | CONTROL_HELLO_ACK | CONTROL_SHUTDOWN | CONTROL_SHUTDOWN_ACK => {
            fixed(ProviderChannel::Control, 0, 0, 0)
        }
        CONTROL_START => fixed(ProviderChannel::Control, 1, 0, 16),
        CONTROL_STARTED => fixed(ProviderChannel::Control, 2, 1, 40),
        CONTROL_START_FAILED => fixed(ProviderChannel::Control, 1, 0, 8),
        CONTROL_STOP => fixed(ProviderChannel::Control, 2, 0, 0),
        CONTROL_STOPPED => fixed(ProviderChannel::Control, 2, 0, 8),
        CONTROL_CANCEL | CONTROL_CANCELLED | CONTROL_TERMINAL => {
            fixed(ProviderChannel::Control, 0, 0, 8)
        }
        DATA_HELLO | DATA_HELLO_ACK | DATA_STOP | DATA_SHUTDOWN | DATA_SHUTDOWN_ACK => {
            fixed(ProviderChannel::Data, 2, 0, 0)
        }
        DATA_READINESS => fixed(ProviderChannel::Data, 2, 0, 16),
        DATA_READ => fixed(ProviderChannel::Data, 2, 0, 8),
        DATA_READ_RESULT => (ProviderChannel::Data, 2, 0, 16, MAX_BODY_BYTES),
        DATA_WRITE => (ProviderChannel::Data, 2, 0, 8 + 4 + 1, MAX_BODY_BYTES - 8),
        DATA_WRITE_RESULT | DATA_OPERATION_FAILED => fixed(ProviderChannel::Data, 2, 0, 16),
        DATA_STOPPED | DATA_TERMINAL => fixed(ProviderChannel::Data, 2, 0, 8),
        _ => return Err(VmnetProviderError::InvalidFrame),
    })
}

fn encode_message(message: &ProviderMessage) -> Result<(u8, Vec<u8>), VmnetProviderError> {
    let mut body = Vec::new();
    let kind = match message {
        ProviderMessage::Control(message) => match message {
            ControlMessage::Hello => CONTROL_HELLO,
            ControlMessage::HelloAck => CONTROL_HELLO_ACK,
            ControlMessage::Start {
                policy_slot,
                requested,
            } => {
                encode_requested(&mut body, *policy_slot, *requested);
                CONTROL_START
            }
            ControlMessage::Started { parameters } => {
                encode_realized(&mut body, *parameters);
                CONTROL_STARTED
            }
            ControlMessage::StartFailed { status } => {
                body.extend_from_slice(&(*status as u16).to_be_bytes());
                body.extend_from_slice(&[0; 6]);
                CONTROL_START_FAILED
            }
            ControlMessage::Stop => CONTROL_STOP,
            ControlMessage::Stopped { cleanup } => {
                body.push(*cleanup as u8);
                body.extend_from_slice(&[0; 7]);
                CONTROL_STOPPED
            }
            ControlMessage::Cancel { reason } => {
                body.push(*reason as u8);
                body.extend_from_slice(&[0; 7]);
                CONTROL_CANCEL
            }
            ControlMessage::Cancelled { cleanup } => {
                body.push(*cleanup as u8);
                body.extend_from_slice(&[0; 7]);
                CONTROL_CANCELLED
            }
            ControlMessage::Shutdown => CONTROL_SHUTDOWN,
            ControlMessage::ShutdownAck => CONTROL_SHUTDOWN_ACK,
            ControlMessage::Terminal { code } => {
                body.push(*code as u8);
                body.extend_from_slice(&[0; 7]);
                CONTROL_TERMINAL
            }
        },
        ProviderMessage::Data(message) => match message {
            DataMessage::Hello => DATA_HELLO,
            DataMessage::HelloAck => DATA_HELLO_ACK,
            DataMessage::Readiness {
                epoch,
                estimated_packets,
            } => {
                body.extend_from_slice(&epoch.get().to_be_bytes());
                body.extend_from_slice(&estimated_packets.to_be_bytes());
                body.extend_from_slice(&[0; 6]);
                DATA_READINESS
            }
            DataMessage::Read { max_packets } => {
                body.extend_from_slice(&max_packets.to_be_bytes());
                body.extend_from_slice(&[0; 6]);
                DATA_READ
            }
            DataMessage::ReadResult { request, packets } => {
                body.extend_from_slice(&request.get().to_be_bytes());
                encode_batch(&mut body, packets)?;
                DATA_READ_RESULT
            }
            DataMessage::Write { packets } => {
                encode_batch(&mut body, packets)?;
                DATA_WRITE
            }
            DataMessage::WriteResult {
                request,
                completed_packets,
            } => {
                body.extend_from_slice(&request.get().to_be_bytes());
                body.extend_from_slice(&completed_packets.to_be_bytes());
                body.extend_from_slice(&[0; 6]);
                DATA_WRITE_RESULT
            }
            DataMessage::OperationFailed {
                request,
                operation,
                status,
            } => {
                body.extend_from_slice(&request.get().to_be_bytes());
                body.push(*operation as u8);
                body.push(0);
                body.extend_from_slice(&(*status as u16).to_be_bytes());
                body.extend_from_slice(&[0; 4]);
                DATA_OPERATION_FAILED
            }
            DataMessage::Stop => DATA_STOP,
            DataMessage::Stopped { cleanup } => {
                body.push(*cleanup as u8);
                body.extend_from_slice(&[0; 7]);
                DATA_STOPPED
            }
            DataMessage::Shutdown => DATA_SHUTDOWN,
            DataMessage::ShutdownAck => DATA_SHUTDOWN_ACK,
            DataMessage::Terminal { code } => {
                body.push(*code as u8);
                body.extend_from_slice(&[0; 7]);
                DATA_TERMINAL
            }
        },
    };
    Ok((kind, body))
}

fn decode_message(
    channel: ProviderChannel,
    kind: u8,
    body: &[u8],
) -> Result<ProviderMessage, VmnetProviderError> {
    let message = match (channel, kind) {
        (ProviderChannel::Control, CONTROL_HELLO) => {
            ProviderMessage::Control(ControlMessage::Hello)
        }
        (ProviderChannel::Control, CONTROL_HELLO_ACK) => {
            ProviderMessage::Control(ControlMessage::HelloAck)
        }
        (ProviderChannel::Control, CONTROL_START) => {
            let (policy_slot, requested) = decode_requested(body)?;
            ProviderMessage::Control(ControlMessage::Start {
                policy_slot,
                requested,
            })
        }
        (ProviderChannel::Control, CONTROL_STARTED) => {
            ProviderMessage::Control(ControlMessage::Started {
                parameters: decode_realized(body)?,
            })
        }
        (ProviderChannel::Control, CONTROL_START_FAILED) => {
            require_zero(body, 2, 8)?;
            ProviderMessage::Control(ControlMessage::StartFailed {
                status: ProviderStatus::decode(read_u16(body, 0)?)?,
            })
        }
        (ProviderChannel::Control, CONTROL_STOP) => ProviderMessage::Control(ControlMessage::Stop),
        (ProviderChannel::Control, CONTROL_STOPPED) => {
            require_zero(body, 1, 8)?;
            ProviderMessage::Control(ControlMessage::Stopped {
                cleanup: ProviderCleanup::decode(read_u8(body, 0)?)?,
            })
        }
        (ProviderChannel::Control, CONTROL_CANCEL) => {
            require_zero(body, 1, 8)?;
            ProviderMessage::Control(ControlMessage::Cancel {
                reason: ProviderCancelReason::decode(read_u8(body, 0)?)?,
            })
        }
        (ProviderChannel::Control, CONTROL_CANCELLED) => {
            require_zero(body, 1, 8)?;
            ProviderMessage::Control(ControlMessage::Cancelled {
                cleanup: ProviderCleanup::decode(read_u8(body, 0)?)?,
            })
        }
        (ProviderChannel::Control, CONTROL_SHUTDOWN) => {
            ProviderMessage::Control(ControlMessage::Shutdown)
        }
        (ProviderChannel::Control, CONTROL_SHUTDOWN_ACK) => {
            ProviderMessage::Control(ControlMessage::ShutdownAck)
        }
        (ProviderChannel::Control, CONTROL_TERMINAL) => {
            require_zero(body, 1, 8)?;
            ProviderMessage::Control(ControlMessage::Terminal {
                code: ProviderTerminalCode::decode(read_u8(body, 0)?)?,
            })
        }
        (ProviderChannel::Data, DATA_HELLO) => ProviderMessage::Data(DataMessage::Hello),
        (ProviderChannel::Data, DATA_HELLO_ACK) => ProviderMessage::Data(DataMessage::HelloAck),
        (ProviderChannel::Data, DATA_READINESS) => {
            require_zero(body, 10, 16)?;
            ProviderMessage::Data(DataMessage::Readiness {
                epoch: VmnetReadinessEpoch::decode(read_u64(body, 0)?)?,
                estimated_packets: read_u16(body, 8)?,
            })
        }
        (ProviderChannel::Data, DATA_READ) => {
            require_zero(body, 2, 8)?;
            ProviderMessage::Data(DataMessage::Read {
                max_packets: read_u16(body, 0)?,
            })
        }
        (ProviderChannel::Data, DATA_READ_RESULT) => {
            ProviderMessage::Data(DataMessage::ReadResult {
                request: VmnetSequence::decode(read_u64(body, 0)?)?,
                packets: decode_batch(
                    body.get(8..).ok_or(VmnetProviderError::InvalidFrame)?,
                    true,
                )?,
            })
        }
        (ProviderChannel::Data, DATA_WRITE) => ProviderMessage::Data(DataMessage::Write {
            packets: decode_batch(body, false)?,
        }),
        (ProviderChannel::Data, DATA_WRITE_RESULT) => {
            require_zero(body, 10, 16)?;
            ProviderMessage::Data(DataMessage::WriteResult {
                request: VmnetSequence::decode(read_u64(body, 0)?)?,
                completed_packets: read_u16(body, 8)?,
            })
        }
        (ProviderChannel::Data, DATA_OPERATION_FAILED) => {
            if read_u8(body, 9)? != 0 {
                return Err(VmnetProviderError::InvalidFrame);
            }
            require_zero(body, 12, 16)?;
            ProviderMessage::Data(DataMessage::OperationFailed {
                request: VmnetSequence::decode(read_u64(body, 0)?)?,
                operation: ProviderOperation::decode(read_u8(body, 8)?)?,
                status: ProviderStatus::decode(read_u16(body, 10)?)?,
            })
        }
        (ProviderChannel::Data, DATA_STOP) => ProviderMessage::Data(DataMessage::Stop),
        (ProviderChannel::Data, DATA_STOPPED) => {
            require_zero(body, 1, 8)?;
            ProviderMessage::Data(DataMessage::Stopped {
                cleanup: ProviderCleanup::decode(read_u8(body, 0)?)?,
            })
        }
        (ProviderChannel::Data, DATA_SHUTDOWN) => ProviderMessage::Data(DataMessage::Shutdown),
        (ProviderChannel::Data, DATA_SHUTDOWN_ACK) => {
            ProviderMessage::Data(DataMessage::ShutdownAck)
        }
        (ProviderChannel::Data, DATA_TERMINAL) => {
            require_zero(body, 1, 8)?;
            ProviderMessage::Data(DataMessage::Terminal {
                code: ProviderTerminalCode::decode(read_u8(body, 0)?)?,
            })
        }
        _ => return Err(VmnetProviderError::InvalidFrame),
    };
    Ok(message)
}

fn encode_requested(
    body: &mut Vec<u8>,
    policy_slot: VmnetPolicySlot,
    requested: RequestedVmnetParameters,
) {
    let flags = (if requested.mac.is_some() {
        REQUEST_MAC_PRESENT
    } else {
        0
    }) | (if requested.mtu.is_some() {
        REQUEST_MTU_PRESENT
    } else {
        0
    });
    body.push(policy_slot as u8);
    body.push(flags);
    body.extend_from_slice(&[0; 2]);
    body.extend_from_slice(&requested.mac.unwrap_or([0; 6]));
    body.extend_from_slice(&requested.mtu.unwrap_or(0).to_be_bytes());
    body.extend_from_slice(&[0; 4]);
}

fn decode_requested(
    body: &[u8],
) -> Result<(VmnetPolicySlot, RequestedVmnetParameters), VmnetProviderError> {
    let flags = read_u8(body, 1)?;
    if flags & !REQUEST_FLAGS != 0 {
        return Err(VmnetProviderError::InvalidFrame);
    }
    require_zero(body, 2, 4)?;
    require_zero(body, 12, 16)?;
    let raw_mac: [u8; 6] = read_array(body, 4)?;
    let raw_mtu = read_u16(body, 10)?;
    if flags & REQUEST_MAC_PRESENT == 0 && raw_mac != [0; 6]
        || flags & REQUEST_MTU_PRESENT == 0 && raw_mtu != 0
    {
        return Err(VmnetProviderError::InvalidFrame);
    }
    let requested = RequestedVmnetParameters::new(
        (flags & REQUEST_MAC_PRESENT != 0).then_some(raw_mac),
        (flags & REQUEST_MTU_PRESENT != 0).then_some(raw_mtu),
    )
    .map_err(|_| VmnetProviderError::InvalidFrame)?;
    Ok((VmnetPolicySlot::decode(read_u8(body, 0)?)?, requested))
}

fn encode_realized(body: &mut Vec<u8>, parameters: RealizedVmnetParameters) {
    let flags = (if parameters.backend_interface_id.is_some() {
        REALIZED_BACKEND_ID_PRESENT
    } else {
        0
    }) | (if parameters.read_max_packets.is_some() {
        REALIZED_READ_MAX_PRESENT
    } else {
        0
    }) | (if parameters.write_max_packets.is_some() {
        REALIZED_WRITE_MAX_PRESENT
    } else {
        0
    }) | (if parameters.direct_virtio_header_available {
        REALIZED_DIRECT_AVAILABLE
    } else {
        0
    }) | (if parameters.direct_virtio_header_enabled {
        REALIZED_DIRECT_ENABLED
    } else {
        0
    });
    body.extend_from_slice(&flags.to_be_bytes());
    body.extend_from_slice(&[0; 2]);
    body.extend_from_slice(&parameters.mac);
    body.extend_from_slice(&parameters.effective_mtu.to_be_bytes());
    body.extend_from_slice(&parameters.maximum_packet_bytes.to_be_bytes());
    body.extend_from_slice(&parameters.backend_interface_id.unwrap_or([0; 16]));
    body.extend_from_slice(&parameters.read_max_packets.unwrap_or(0).to_be_bytes());
    body.extend_from_slice(&parameters.write_max_packets.unwrap_or(0).to_be_bytes());
    body.extend_from_slice(&[0; 4]);
}

fn decode_realized(body: &[u8]) -> Result<RealizedVmnetParameters, VmnetProviderError> {
    let flags = read_u16(body, 0)?;
    if flags & !REALIZED_FLAGS != 0 {
        return Err(VmnetProviderError::InvalidFrame);
    }
    require_zero(body, 2, 4)?;
    require_zero(body, 36, 40)?;
    let backend_id: [u8; 16] = read_array(body, 16)?;
    let read_max = read_u16(body, 32)?;
    let write_max = read_u16(body, 34)?;
    if flags & REALIZED_BACKEND_ID_PRESENT == 0 && backend_id != [0; 16]
        || flags & REALIZED_READ_MAX_PRESENT == 0 && read_max != 0
        || flags & REALIZED_WRITE_MAX_PRESENT == 0 && write_max != 0
    {
        return Err(VmnetProviderError::InvalidFrame);
    }
    RealizedVmnetParameters {
        mac: read_array(body, 4)?,
        effective_mtu: read_u16(body, 10)?,
        maximum_packet_bytes: read_u32(body, 12)?,
        backend_interface_id: (flags & REALIZED_BACKEND_ID_PRESENT != 0).then_some(backend_id),
        read_max_packets: (flags & REALIZED_READ_MAX_PRESENT != 0).then_some(read_max),
        write_max_packets: (flags & REALIZED_WRITE_MAX_PRESENT != 0).then_some(write_max),
        direct_virtio_header_available: flags & REALIZED_DIRECT_AVAILABLE != 0,
        direct_virtio_header_enabled: flags & REALIZED_DIRECT_ENABLED != 0,
    }
    .validated()
    .map_err(|_| VmnetProviderError::InvalidFrame)
}

fn encode_batch(body: &mut Vec<u8>, batch: &VmnetPacketBatch) -> Result<(), VmnetProviderError> {
    body.extend_from_slice(
        &u16::try_from(batch.lengths.len())
            .map_err(|_| VmnetProviderError::LimitExceeded)?
            .to_be_bytes(),
    );
    body.extend_from_slice(&[0; 2]);
    body.extend_from_slice(
        &u32::try_from(batch.bytes.len())
            .map_err(|_| VmnetProviderError::LimitExceeded)?
            .to_be_bytes(),
    );
    for length in &batch.lengths {
        body.extend_from_slice(&length.to_be_bytes());
    }
    body.extend_from_slice(&batch.bytes);
    Ok(())
}

fn decode_batch(body: &[u8], allow_empty: bool) -> Result<VmnetPacketBatch, VmnetProviderError> {
    if body.len() < 8 || read_u16(body, 2)? != 0 {
        return Err(VmnetProviderError::InvalidFrame);
    }
    let count = usize::from(read_u16(body, 0)?);
    let aggregate =
        usize::try_from(read_u32(body, 4)?).map_err(|_| VmnetProviderError::InvalidFrame)?;
    if count > MAX_PACKET_COUNT || aggregate > MAX_AGGREGATE_PACKET_BYTES {
        return Err(VmnetProviderError::LimitExceeded);
    }
    let metadata_end = 8_usize
        .checked_add(
            count
                .checked_mul(4)
                .ok_or(VmnetProviderError::LimitExceeded)?,
        )
        .ok_or(VmnetProviderError::LimitExceeded)?;
    let total = metadata_end
        .checked_add(aggregate)
        .ok_or(VmnetProviderError::LimitExceeded)?;
    if total != body.len() {
        return Err(VmnetProviderError::InvalidFrame);
    }
    let mut lengths = Vec::new();
    lengths
        .try_reserve_exact(count)
        .map_err(|_| VmnetProviderError::LimitExceeded)?;
    for index in 0..count {
        let offset = 8_usize
            .checked_add(
                index
                    .checked_mul(4)
                    .ok_or(VmnetProviderError::LimitExceeded)?,
            )
            .ok_or(VmnetProviderError::LimitExceeded)?;
        lengths.push(read_u32(body, offset)?);
    }
    let bytes = body
        .get(metadata_end..)
        .ok_or(VmnetProviderError::InvalidFrame)?
        .to_vec();
    VmnetPacketBatch::from_parts(lengths, bytes, allow_empty)
}

fn require_zero(bytes: &[u8], start: usize, end: usize) -> Result<(), VmnetProviderError> {
    bytes
        .get(start..end)
        .filter(|reserved| reserved.iter().all(|byte| *byte == 0))
        .map(|_| ())
        .ok_or(VmnetProviderError::InvalidFrame)
}

fn read_u8(bytes: &[u8], offset: usize) -> Result<u8, VmnetProviderError> {
    bytes
        .get(offset)
        .copied()
        .ok_or(VmnetProviderError::InvalidFrame)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, VmnetProviderError> {
    Ok(u16::from_be_bytes(read_array(bytes, offset)?))
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, VmnetProviderError> {
    Ok(u32::from_be_bytes(read_array(bytes, offset)?))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, VmnetProviderError> {
    Ok(u64::from_be_bytes(read_array(bytes, offset)?))
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], VmnetProviderError> {
    bytes
        .get(offset..offset.saturating_add(N))
        .and_then(|slice| slice.try_into().ok())
        .ok_or(VmnetProviderError::InvalidFrame)
}

/// Bounded incremental provider-frame decoder.
#[derive(Debug, Default)]
pub struct ProviderFrameDecoder {
    buffered: Vec<u8>,
    poisoned: bool,
}

impl ProviderFrameDecoder {
    /// Adds bytes and returns every newly complete frame in order.
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<ProviderFrame>, VmnetProviderError> {
        if self.poisoned {
            return Err(VmnetProviderError::Poisoned);
        }
        let result = self.push_inner(bytes);
        if result.is_err() {
            self.buffered.clear();
            self.poisoned = true;
        }
        result
    }

    fn push_inner(&mut self, bytes: &[u8]) -> Result<Vec<ProviderFrame>, VmnetProviderError> {
        self.buffered
            .len()
            .checked_add(bytes.len())
            .filter(|required| *required <= MAX_BUFFERED_FRAME_BYTES)
            .ok_or(VmnetProviderError::LimitExceeded)?;
        self.buffered
            .try_reserve(bytes.len())
            .map_err(|_| VmnetProviderError::LimitExceeded)?;
        self.buffered.extend_from_slice(bytes);
        let mut frames = Vec::new();
        loop {
            if self.buffered.len() < HEADER_BYTES {
                break;
            }
            let header = decode_header(
                self.buffered
                    .get(..HEADER_BYTES)
                    .ok_or(VmnetProviderError::InvalidFrame)?,
            )?;
            let total = HEADER_BYTES
                .checked_add(header.body_len())
                .filter(|total| *total <= MAX_FRAME_BYTES)
                .ok_or(VmnetProviderError::LimitExceeded)?;
            if self.buffered.len() < total {
                break;
            }
            let frame = decode_frame(
                self.buffered
                    .get(..total)
                    .ok_or(VmnetProviderError::InvalidFrame)?,
            )?;
            self.buffered.drain(..total);
            frames.push(frame);
        }
        Ok(frames)
    }

    /// Returns whether malformed input permanently poisoned the decoder.
    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> SessionId {
        SessionId::from_bytes([0x31; 32])
    }

    fn interface() -> VmnetInterfaceId {
        VmnetInterfaceId::new(7).expect("interface should validate")
    }

    fn generation() -> VmnetGeneration {
        VmnetGeneration::new(9).expect("generation should validate")
    }

    fn sequence(value: u64) -> VmnetSequence {
        VmnetSequence::new(value).expect("sequence should validate")
    }

    fn requested() -> RequestedVmnetParameters {
        RequestedVmnetParameters::new(Some([2, 1, 2, 3, 4, 5]), Some(1500))
            .expect("request should validate")
    }

    fn realized() -> RealizedVmnetParameters {
        RealizedVmnetParameters::new([2, 6, 7, 8, 9, 10], 1500, 2048)
            .expect("base parameters should validate")
            .with_backend_interface_id(Some([4; 16]))
            .expect("identity should validate")
            .with_batch_limits(Some(4), Some(5))
            .expect("batch limits should validate")
            .with_direct_virtio_header(true, false)
            .expect("parameters should validate")
    }

    fn frames() -> Vec<ProviderFrame> {
        let empty = VmnetPacketBatch::read(&[]).expect("empty read should validate");
        let packets =
            VmnetPacketBatch::write(&[&[1, 2], &[3, 4, 5]]).expect("packets should validate");
        let control = [
            (None, None, ControlMessage::Hello),
            (None, None, ControlMessage::HelloAck),
            (
                Some(interface()),
                None,
                ControlMessage::Start {
                    policy_slot: VmnetPolicySlot::Shared,
                    requested: requested(),
                },
            ),
            (
                Some(interface()),
                Some(generation()),
                ControlMessage::Started {
                    parameters: realized(),
                },
            ),
            (
                Some(interface()),
                None,
                ControlMessage::StartFailed {
                    status: ProviderStatus::NotAuthorized,
                },
            ),
            (Some(interface()), Some(generation()), ControlMessage::Stop),
            (
                Some(interface()),
                Some(generation()),
                ControlMessage::Stopped {
                    cleanup: ProviderCleanup::Complete,
                },
            ),
            (
                None,
                None,
                ControlMessage::Cancel {
                    reason: ProviderCancelReason::Worker,
                },
            ),
            (
                None,
                None,
                ControlMessage::Cancelled {
                    cleanup: ProviderCleanup::Complete,
                },
            ),
            (None, None, ControlMessage::Shutdown),
            (None, None, ControlMessage::ShutdownAck),
            (
                None,
                None,
                ControlMessage::Terminal {
                    code: ProviderTerminalCode::Protocol,
                },
            ),
        ];
        let mut result = Vec::new();
        for (index, (iface, generation, message)) in control.into_iter().enumerate() {
            result.push(
                ProviderFrame::control(
                    session(),
                    iface,
                    generation,
                    sequence(u64::try_from(index + 1).expect("index should fit")),
                    message,
                )
                .expect("control frame should validate"),
            );
        }
        let data = [
            DataMessage::Hello,
            DataMessage::HelloAck,
            DataMessage::Readiness {
                epoch: VmnetReadinessEpoch::new(1).expect("epoch should validate"),
                estimated_packets: 2,
            },
            DataMessage::Read { max_packets: 4 },
            DataMessage::ReadResult {
                request: sequence(4),
                packets: empty,
            },
            DataMessage::Write {
                packets: packets.clone(),
            },
            DataMessage::WriteResult {
                request: sequence(6),
                completed_packets: 1,
            },
            DataMessage::OperationFailed {
                request: sequence(6),
                operation: ProviderOperation::Write,
                status: ProviderStatus::BufferExhausted,
            },
            DataMessage::Stop,
            DataMessage::Stopped {
                cleanup: ProviderCleanup::Complete,
            },
            DataMessage::Shutdown,
            DataMessage::ShutdownAck,
            DataMessage::Terminal {
                code: ProviderTerminalCode::Cleanup,
            },
        ];
        for (index, message) in data.into_iter().enumerate() {
            result.push(
                ProviderFrame::data(
                    session(),
                    interface(),
                    generation(),
                    sequence(u64::try_from(index + 20).expect("index should fit")),
                    message,
                )
                .expect("data frame should validate"),
            );
        }
        result
    }

    #[test]
    fn every_message_round_trips_and_descriptor_contract_is_exact() {
        for frame in frames() {
            let encoded = encode_frame(&frame).expect("frame should encode");
            assert!(encoded.len() <= MAX_FRAME_BYTES);
            assert_eq!(decode_frame(&encoded), Ok(frame.clone()));
            assert_eq!(
                decode_header(encoded.get(..HEADER_BYTES).expect("header should exist"))
                    .expect("header should decode")
                    .descriptor_count(),
                frame.descriptor_count()
            );
        }
    }

    #[test]
    fn representative_started_header_is_golden() {
        let frame = ProviderFrame::control(
            session(),
            Some(interface()),
            Some(generation()),
            sequence(3),
            ControlMessage::Started {
                parameters: realized(),
            },
        )
        .expect("frame should validate");
        let encoded = encode_frame(&frame).expect("frame should encode");
        assert_eq!(
            encoded.get(0..17),
            Some(
                &[
                    b'B',
                    b'B',
                    b'V',
                    b'N',
                    b'E',
                    b'T',
                    b'P',
                    0,
                    0,
                    1,
                    CONTROL_CHANNEL,
                    CONTROL_STARTED,
                    0,
                    0,
                    0,
                    40,
                    1,
                ][..]
            )
        );
        assert_eq!(encoded.get(17..24), Some(&[0; 7][..]));
        assert_eq!(encoded.get(56..60), Some(&7_u32.to_be_bytes()[..]));
        assert_eq!(encoded.get(64..72), Some(&9_u64.to_be_bytes()[..]));
        assert_eq!(encoded.get(72..80), Some(&3_u64.to_be_bytes()[..]));
    }

    #[test]
    fn decoder_accepts_every_split_and_coalesced_frames() {
        let selected = frames();
        let first = encode_frame(selected.first().expect("first frame should exist"))
            .expect("frame should encode");
        for split in 0..first.len() {
            let mut decoder = ProviderFrameDecoder::default();
            assert!(
                decoder
                    .push(first.get(..split).expect("prefix should exist"))
                    .expect("prefix should decode")
                    .is_empty()
            );
            let decoded = decoder
                .push(first.get(split..).expect("suffix should exist"))
                .expect("suffix should decode");
            assert_eq!(
                decoded,
                vec![selected.first().expect("frame should exist").clone()]
            );
        }
        let mut all = Vec::new();
        for frame in &selected {
            all.extend_from_slice(&encode_frame(frame).expect("frame should encode"));
        }
        let mut decoder = ProviderFrameDecoder::default();
        assert_eq!(decoder.push(&all), Ok(selected));
    }

    #[test]
    fn header_body_and_reserved_corruption_fail_closed() {
        let frame = frames().remove(2);
        let encoded = encode_frame(&frame).expect("frame should encode");
        for offset in [0, 8, 10, 11, 16, 17, 60] {
            let mut corrupt = encoded.clone();
            let byte = corrupt.get_mut(offset).expect("offset should exist");
            *byte ^= 0xff;
            assert!(decode_frame(&corrupt).is_err(), "offset {offset}");
        }
        let mut zero_sequence = encoded.clone();
        zero_sequence
            .get_mut(72..80)
            .expect("sequence range should exist")
            .fill(0);
        assert_eq!(
            decode_frame(&zero_sequence),
            Err(VmnetProviderError::InvalidFrame)
        );
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert_eq!(
            decode_frame(&trailing),
            Err(VmnetProviderError::InvalidFrame)
        );
        for split in 0..encoded.len() {
            assert!(decode_frame(encoded.get(..split).expect("prefix should exist")).is_err());
        }
    }

    #[test]
    fn packet_and_parameter_bounds_are_checked() {
        assert!(VmnetPacketBatch::write(&[]).is_err());
        assert!(VmnetPacketBatch::read(&[]).is_ok());
        let oversized = vec![0_u8; MAX_PACKET_BUFFER_BYTES + 1];
        assert!(VmnetPacketBatch::write(&[&oversized]).is_err());
        let packet = vec![0_u8; MAX_PACKET_BUFFER_BYTES];
        assert!(VmnetPacketBatch::write(&[&packet, &packet, &packet, &packet]).is_err());
        assert!(RequestedVmnetParameters::new(Some([1; 6]), None).is_err());
        assert!(RealizedVmnetParameters::new([2; 6], 1500, 0).is_err());
        assert!(
            RealizedVmnetParameters::new([2; 6], 1500, 2048)
                .expect("base parameters should validate")
                .with_direct_virtio_header(false, true)
                .is_err()
        );
    }

    #[test]
    fn debug_surfaces_redact_protocol_values_and_packet_bytes() {
        let frame = frames().remove(2);
        let debug = format!("{frame:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("1500"));
        assert_eq!(
            VmnetProviderError::InvalidFrame.to_string(),
            "private vmnet provider protocol failure"
        );
    }
}
