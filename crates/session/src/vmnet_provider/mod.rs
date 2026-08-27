//! Closed, bounded vmnet-provider protocol over already-connected Unix streams.
//!
//! The protocol is bound to the private launcher-worker lifecycle session. It
//! carries fixed policy slots and packet data, but no path, bridge name,
//! command, credential, process launch, privilege transition, or vmnet object.

mod frame;
mod state;
mod transport;

use std::fmt;
use std::io;

pub use frame::{
    CONTROL_CHANNEL, ControlMessage, DATA_CHANNEL, DataMessage, HEADER_BYTES,
    MAX_ACTIVE_INTERFACES, MAX_AGGREGATE_PACKET_BYTES, MAX_BUFFERED_FRAME_BYTES, MAX_FRAME_BYTES,
    MAX_PACKET_BUFFER_BYTES, MAX_PACKET_COUNT, ProviderCancelReason, ProviderChannel,
    ProviderCleanup, ProviderEnvelope, ProviderFrame, ProviderFrameDecoder, ProviderFrameHeader,
    ProviderOperation, ProviderStatus, ProviderTerminalCode, RealizedVmnetParameters,
    RequestedVmnetParameters, VmnetGeneration, VmnetInterfaceId, VmnetPacketBatch, VmnetPolicySlot,
    VmnetReadinessEpoch, VmnetSequence, decode_frame, decode_header, encode_frame,
};
pub use state::{
    ControlBrokerEvent, ControlBrokerState, ControlClientEvent, ControlClientState,
    DataClientEvent, DataClientState, DataOwnerEvent, DataOwnerState, VmnetControlBroker,
    VmnetControlClient, VmnetDataClient, VmnetDataOwner,
};
pub use transport::{MAX_PROVIDER_TIMEOUT, VmnetProviderTransport};

/// Redacted vmnet-provider protocol failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetProviderError {
    /// A local constructor or timeout configuration is invalid.
    InvalidConfiguration,
    /// Wire bytes or descriptor shape are malformed or noncanonical.
    InvalidFrame,
    /// A fixed count or byte bound was exceeded.
    LimitExceeded,
    /// A local role attempted an operation outside its legal lifecycle.
    InvalidLifecycle,
    /// A peer frame violated ordering, correlation, identity, or scope.
    InvalidPeerState,
    /// A transport has already entered terminal poison state.
    Poisoned,
    /// The operation deadline elapsed.
    Timeout,
    /// The peer closed before a requested frame began.
    Disconnected,
    /// The peer closed after transferring only a frame prefix.
    UnexpectedEof,
    /// A local socket operation failed.
    Io(io::ErrorKind),
}

impl fmt::Display for VmnetProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("private vmnet provider protocol failure")
    }
}

impl std::error::Error for VmnetProviderError {}

impl From<bangbang_unix_stream::UnixStreamTransportError> for VmnetProviderError {
    fn from(error: bangbang_unix_stream::UnixStreamTransportError) -> Self {
        match error {
            bangbang_unix_stream::UnixStreamTransportError::Invalid => Self::InvalidFrame,
            bangbang_unix_stream::UnixStreamTransportError::Timeout => Self::Timeout,
            bangbang_unix_stream::UnixStreamTransportError::Disconnected => Self::Disconnected,
            bangbang_unix_stream::UnixStreamTransportError::UnexpectedEof => Self::UnexpectedEof,
            bangbang_unix_stream::UnixStreamTransportError::Io(kind) => Self::Io(kind),
        }
    }
}
