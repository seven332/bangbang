//! Fixed bootstrap and supervision protocol for one elevated vmnet topology.
//!
//! The root provider and ordinary launcher exchange only exact process,
//! lifecycle, and policy state here. Provider-v1 remains the separate control
//! and packet protocol.

use std::fmt;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::credential::{CredentialMode, CredentialTarget};
use crate::{MAX_VMNET_BRIDGE_NAME_BYTES, MAX_VMNET_BRIDGE_NAMES, SessionId, VmnetAuthority};

/// Exact encoded size of one topology frame.
pub const VMNET_TOPOLOGY_FRAME_BYTES: usize = 192;

/// Fixed launcher-side topology descriptor inherited from the provider.
pub const VMNET_TOPOLOGY_FD: libc::c_int = 8;

/// Fixed launcher-side provider descriptor inherited from the provider.
pub const VMNET_TOPOLOGY_PROVIDER_FD: libc::c_int = 9;

/// Private marker for one provider-created ordinary launcher.
pub const VMNET_TOPOLOGY_ENV_KEY: &str = "BANGBANG_INTERNAL_VMNET_TOPOLOGY_V1";

/// Exact value required for [`VMNET_TOPOLOGY_ENV_KEY`].
pub const VMNET_TOPOLOGY_ENV_VALUE: &str = "1";

const MAGIC: [u8; 8] = *b"BBVNTP1\0";
const VERSION: u16 = 1;
const KIND_START: u8 = 1;
const KIND_DROPPED: u8 = 2;
const KIND_DROP_ACK: u8 = 3;
const KIND_OUTER_START: u8 = 4;
const KIND_OUTER_HELLO: u8 = 5;
const KIND_PROCEED: u8 = 6;
const KIND_ACTIVATE: u8 = 7;
const KIND_BROKER_READY: u8 = 8;
const KIND_LAUNCHER_READY: u8 = 9;
const KIND_READY_ACK: u8 = 10;
const KIND_CANCEL: u8 = 11;
const KIND_TERMINAL: u8 = 12;
const KIND_TERMINAL_ACK: u8 = 13;
const FLAG_HOST: u8 = 1 << 0;
const FLAG_SHARED: u8 = 1 << 1;
const AUTHORITY_FLAGS: u8 = FLAG_HOST | FLAG_SHARED;
const BRIDGE_SLOT_BYTES: usize = MAX_VMNET_BRIDGE_NAME_BYTES + 1;

/// Redacted topology protocol failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VmnetTopologyError;

impl fmt::Display for VmnetTopologyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid private vmnet topology frame")
    }
}

impl std::error::Error for VmnetTopologyError {}

/// Bounded topology transport failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmnetTopologyTransportError {
    /// A fixed read or write deadline elapsed.
    Timeout,
    /// The peer closed before one complete frame arrived.
    Disconnected,
    /// One complete frame was noncanonical.
    Invalid,
    /// One local I/O operation failed.
    Io(io::ErrorKind),
}

/// Sole-owner fixed-frame transport for one topology supervision stream.
pub struct VmnetTopologyTransport {
    stream: UnixStream,
}

impl VmnetTopologyTransport {
    /// Constructs a blocking bounded transport.
    pub fn new(stream: UnixStream, timeout: Duration) -> Result<Self, VmnetTopologyTransportError> {
        if timeout.is_zero() {
            return Err(VmnetTopologyTransportError::Invalid);
        }
        stream.set_nonblocking(false).map_err(map_transport_io)?;
        stream
            .set_read_timeout(Some(timeout))
            .map_err(map_transport_io)?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(map_transport_io)?;
        Ok(Self { stream })
    }

    /// Returns the live descriptor for bounded poll integration.
    #[must_use]
    pub fn as_raw_fd(&self) -> RawFd {
        self.stream.as_raw_fd()
    }

    /// Sends one canonical frame without descriptors.
    pub fn send(
        &mut self,
        message: VmnetTopologyMessage,
    ) -> Result<(), VmnetTopologyTransportError> {
        let encoded = encode_vmnet_topology_message(message)
            .map_err(|_| VmnetTopologyTransportError::Invalid)?;
        self.stream.write_all(&encoded).map_err(map_transport_io)
    }

    /// Receives exactly one canonical frame without buffering a second.
    pub fn receive(&mut self) -> Result<VmnetTopologyMessage, VmnetTopologyTransportError> {
        let mut encoded = [0_u8; VMNET_TOPOLOGY_FRAME_BYTES];
        self.stream
            .read_exact(&mut encoded)
            .map_err(map_transport_io)?;
        decode_vmnet_topology_message(&encoded).map_err(|_| VmnetTopologyTransportError::Invalid)
    }

    /// Shuts down both directions without exposing the descriptor.
    pub fn shutdown(&self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }

    /// Returns the connected stream to an exact next owner.
    #[must_use]
    pub fn into_stream(self) -> UnixStream {
        self.stream
    }
}

impl fmt::Debug for VmnetTopologyTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VmnetTopologyTransport(<redacted>)")
    }
}

fn map_transport_io(error: io::Error) -> VmnetTopologyTransportError {
    match error.kind() {
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => VmnetTopologyTransportError::Timeout,
        io::ErrorKind::UnexpectedEof
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::ConnectionReset => VmnetTopologyTransportError::Disconnected,
        kind => VmnetTopologyTransportError::Io(kind),
    }
}

/// Foreground or provider-owned daemon lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VmnetTopologyMode {
    /// The sudo caller remains attached until topology termination.
    Foreground = 1,
    /// A same-image handoff owns one detached root broker.
    Daemon = 2,
}

impl VmnetTopologyMode {
    fn decode(value: u8) -> Result<Self, VmnetTopologyError> {
        match value {
            1 => Ok(Self::Foreground),
            2 => Ok(Self::Daemon),
            _ => Err(VmnetTopologyError),
        }
    }

    /// Returns whether the provider owns daemonization for this launch.
    #[must_use]
    pub const fn is_daemon(self) -> bool {
        matches!(self, Self::Daemon)
    }
}

/// Stable value-free cancellation reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VmnetTopologyCancelReason {
    /// The provider received an operator signal.
    Signal = 1,
    /// The launcher disappeared or rejected activation.
    Launcher = 2,
    /// Provider-v1 or an owner failed.
    Provider = 3,
    /// A bounded deadline elapsed.
    Timeout = 4,
    /// A topology protocol invariant failed.
    Protocol = 5,
}

impl VmnetTopologyCancelReason {
    fn decode(value: u8) -> Result<Self, VmnetTopologyError> {
        match value {
            1 => Ok(Self::Signal),
            2 => Ok(Self::Launcher),
            3 => Ok(Self::Provider),
            4 => Ok(Self::Timeout),
            5 => Ok(Self::Protocol),
            _ => Err(VmnetTopologyError),
        }
    }
}

/// Stable value-free terminal result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum VmnetTopologyTerminal {
    /// Worker and provider completed with exact cleanup.
    Complete = 1,
    /// The ordinary launcher reported a non-success result.
    Launcher = 2,
    /// Provider or owner cleanup was uncertain.
    Provider = 3,
    /// Bootstrap or topology validation failed.
    Bootstrap = 4,
    /// A bounded deadline elapsed.
    Timeout = 5,
}

impl VmnetTopologyTerminal {
    fn decode(value: u8) -> Result<Self, VmnetTopologyError> {
        match value {
            1 => Ok(Self::Complete),
            2 => Ok(Self::Launcher),
            3 => Ok(Self::Provider),
            4 => Ok(Self::Bootstrap),
            5 => Ok(Self::Timeout),
            _ => Err(VmnetTopologyError),
        }
    }
}

/// Immutable process scope shared by every frame in one topology.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct VmnetTopologyContext {
    correlation: SessionId,
    target: CredentialTarget,
    launcher_pid: u32,
    mode: VmnetTopologyMode,
}

impl VmnetTopologyContext {
    /// Constructs one nonroot, process-bound topology scope.
    pub fn new(
        correlation: SessionId,
        target: CredentialTarget,
        launcher_pid: u32,
        mode: VmnetTopologyMode,
    ) -> Result<Self, VmnetTopologyError> {
        if correlation.is_pre_session()
            || target.mode() != CredentialMode::Transition
            || launcher_pid == 0
            || launcher_pid > i32::MAX as u32
        {
            return Err(VmnetTopologyError);
        }
        Ok(Self {
            correlation,
            target,
            launcher_pid,
            mode,
        })
    }

    /// Returns the bootstrap correlation identity.
    #[must_use]
    pub const fn correlation(self) -> SessionId {
        self.correlation
    }

    /// Returns the exact nonroot launcher target.
    #[must_use]
    pub const fn target(self) -> CredentialTarget {
        self.target
    }

    /// Returns the exact transition/launcher PID.
    #[must_use]
    pub const fn launcher_pid(self) -> u32 {
        self.launcher_pid
    }

    /// Returns the fixed foreground/daemon mode.
    #[must_use]
    pub const fn mode(self) -> VmnetTopologyMode {
        self.mode
    }
}

impl fmt::Debug for VmnetTopologyContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VmnetTopologyContext(<redacted>)")
    }
}

/// One canonical topology message.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VmnetTopologyMessage {
    /// Root authorizes one minimal transition process.
    Start(VmnetTopologyContext),
    /// The transition process completed and attested credential drop.
    Dropped(VmnetTopologyContext),
    /// Root independently observed the dropped process.
    DropAck(VmnetTopologyContext),
    /// Root leaves one context challenge for the post-exec outer.
    OuterStart(VmnetTopologyContext),
    /// The ordinary outer started from the fixed image.
    OuterHello(VmnetTopologyContext),
    /// Root independently observed the ordinary outer.
    Proceed(VmnetTopologyContext),
    /// Ordinary launcher binds the lifecycle session and fixed policy.
    Activate {
        /// Common process scope.
        context: VmnetTopologyContext,
        /// Exact launcher-worker lifecycle session.
        session: SessionId,
        /// Exact root broker admission policy.
        authority: VmnetAuthority,
    },
    /// Root constructed and froze the broker ledger.
    BrokerReady {
        /// Common process scope.
        context: VmnetTopologyContext,
        /// Exact lifecycle session.
        session: SessionId,
    },
    /// Ordinary launcher completed worker lifecycle readiness.
    LauncherReady {
        /// Common process scope.
        context: VmnetTopologyContext,
        /// Exact lifecycle session.
        session: SessionId,
    },
    /// Root acknowledges the ready topology and optional daemon handoff.
    ReadyAck {
        /// Common process scope.
        context: VmnetTopologyContext,
        /// Exact lifecycle session.
        session: SessionId,
    },
    /// Either endpoint cancels the topology.
    Cancel {
        /// Common process scope.
        context: VmnetTopologyContext,
        /// Established session, or pre-session during bootstrap.
        session: SessionId,
        /// Stable cancellation category.
        reason: VmnetTopologyCancelReason,
    },
    /// Ordinary launcher reports its final bounded result.
    Terminal {
        /// Common process scope.
        context: VmnetTopologyContext,
        /// Exact established session.
        session: SessionId,
        /// Stable terminal category.
        result: VmnetTopologyTerminal,
    },
    /// Root confirms broker/owner cleanup and launcher observation.
    TerminalAck {
        /// Common process scope.
        context: VmnetTopologyContext,
        /// Exact established session.
        session: SessionId,
        /// Stable terminal category.
        result: VmnetTopologyTerminal,
    },
}

impl VmnetTopologyMessage {
    /// Returns the common process scope.
    #[must_use]
    pub const fn context(self) -> VmnetTopologyContext {
        match self {
            Self::Start(context)
            | Self::Dropped(context)
            | Self::DropAck(context)
            | Self::OuterStart(context)
            | Self::OuterHello(context)
            | Self::Proceed(context) => context,
            Self::Activate { context, .. }
            | Self::BrokerReady { context, .. }
            | Self::LauncherReady { context, .. }
            | Self::ReadyAck { context, .. }
            | Self::Cancel { context, .. }
            | Self::Terminal { context, .. }
            | Self::TerminalAck { context, .. } => context,
        }
    }

    /// Returns the fixed sequence number.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        match self {
            Self::Start(_) => 0,
            Self::Dropped(_) => 1,
            Self::DropAck(_) => 2,
            Self::OuterStart(_) => 3,
            Self::OuterHello(_) => 4,
            Self::Proceed(_) => 5,
            Self::Activate { .. } => 6,
            Self::BrokerReady { .. } => 7,
            Self::LauncherReady { .. } => 8,
            Self::ReadyAck { .. } => 9,
            Self::Cancel { .. } | Self::Terminal { .. } => 10,
            Self::TerminalAck { .. } => 11,
        }
    }

    /// Returns the established session, if one is present.
    #[must_use]
    pub const fn session(self) -> SessionId {
        match self {
            Self::Start(_)
            | Self::Dropped(_)
            | Self::DropAck(_)
            | Self::OuterStart(_)
            | Self::OuterHello(_)
            | Self::Proceed(_) => SessionId::pre_session(),
            Self::Activate { session, .. }
            | Self::BrokerReady { session, .. }
            | Self::LauncherReady { session, .. }
            | Self::ReadyAck { session, .. }
            | Self::Cancel { session, .. }
            | Self::Terminal { session, .. }
            | Self::TerminalAck { session, .. } => session,
        }
    }

    fn validated(self) -> Result<Self, VmnetTopologyError> {
        match self {
            Self::Activate {
                session, authority, ..
            } if !session.is_pre_session() && !authority.is_denied() => Ok(self),
            Self::BrokerReady { session, .. }
            | Self::LauncherReady { session, .. }
            | Self::ReadyAck { session, .. }
            | Self::Terminal { session, .. }
            | Self::TerminalAck { session, .. }
                if !session.is_pre_session() =>
            {
                Ok(self)
            }
            Self::Cancel { .. }
            | Self::Start(_)
            | Self::Dropped(_)
            | Self::DropAck(_)
            | Self::OuterStart(_)
            | Self::OuterHello(_)
            | Self::Proceed(_) => Ok(self),
            Self::Activate { .. }
            | Self::BrokerReady { .. }
            | Self::LauncherReady { .. }
            | Self::ReadyAck { .. }
            | Self::Terminal { .. }
            | Self::TerminalAck { .. } => Err(VmnetTopologyError),
        }
    }
}

impl fmt::Debug for VmnetTopologyMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("VmnetTopologyMessage(<redacted>)")
    }
}

/// Encodes one exact topology frame.
pub fn encode_vmnet_topology_message(
    message: VmnetTopologyMessage,
) -> Result<[u8; VMNET_TOPOLOGY_FRAME_BYTES], VmnetTopologyError> {
    let message = message.validated()?;
    let context = message.context();
    let mut encoded = [0_u8; VMNET_TOPOLOGY_FRAME_BYTES];
    encoded[..8].copy_from_slice(&MAGIC);
    encoded[8..10].copy_from_slice(&VERSION.to_be_bytes());
    encoded[10] = kind(message);
    encoded[11] = context.mode() as u8;
    encoded[16..24].copy_from_slice(&message.sequence().to_be_bytes());
    encoded[24..56].copy_from_slice(context.correlation().as_bytes());
    encoded[56..88].copy_from_slice(message.session().as_bytes());
    encoded[88..92].copy_from_slice(&context.target().uid().to_be_bytes());
    encoded[92..96].copy_from_slice(&context.target().gid().to_be_bytes());
    encoded[96..100].copy_from_slice(&context.launcher_pid().to_be_bytes());
    match message {
        VmnetTopologyMessage::Activate { authority, .. } => {
            encode_authority(&mut encoded, authority)?;
        }
        VmnetTopologyMessage::Cancel { reason, .. } => encoded[103] = reason as u8,
        VmnetTopologyMessage::Terminal { result, .. }
        | VmnetTopologyMessage::TerminalAck { result, .. } => encoded[103] = result as u8,
        VmnetTopologyMessage::Start(_)
        | VmnetTopologyMessage::Dropped(_)
        | VmnetTopologyMessage::DropAck(_)
        | VmnetTopologyMessage::OuterStart(_)
        | VmnetTopologyMessage::OuterHello(_)
        | VmnetTopologyMessage::Proceed(_)
        | VmnetTopologyMessage::BrokerReady { .. }
        | VmnetTopologyMessage::LauncherReady { .. }
        | VmnetTopologyMessage::ReadyAck { .. } => {}
    }
    Ok(encoded)
}

/// Decodes, validates, and canonicalizes one exact topology frame.
pub fn decode_vmnet_topology_message(
    encoded: &[u8],
) -> Result<VmnetTopologyMessage, VmnetTopologyError> {
    if encoded.len() != VMNET_TOPOLOGY_FRAME_BYTES
        || encoded.get(..8) != Some(MAGIC.as_slice())
        || read_u16(encoded, 8)? != VERSION
        || encoded.get(12..16).is_none_or(|value| value != [0; 4])
        || encoded.get(168..).is_none_or(|value| value != [0; 24])
    {
        return Err(VmnetTopologyError);
    }
    let mode = VmnetTopologyMode::decode(*encoded.get(11).ok_or(VmnetTopologyError)?)?;
    let correlation = SessionId::from_bytes(read_array(encoded, 24)?);
    let session = SessionId::from_bytes(read_array(encoded, 56)?);
    let target = CredentialTarget::new(read_u32(encoded, 88)?, read_u32(encoded, 92)?)
        .map_err(|_| VmnetTopologyError)?;
    let context = VmnetTopologyContext::new(correlation, target, read_u32(encoded, 96)?, mode)?;
    let frame = match *encoded.get(10).ok_or(VmnetTopologyError)? {
        KIND_START => VmnetTopologyMessage::Start(context),
        KIND_DROPPED => VmnetTopologyMessage::Dropped(context),
        KIND_DROP_ACK => VmnetTopologyMessage::DropAck(context),
        KIND_OUTER_START => VmnetTopologyMessage::OuterStart(context),
        KIND_OUTER_HELLO => VmnetTopologyMessage::OuterHello(context),
        KIND_PROCEED => VmnetTopologyMessage::Proceed(context),
        KIND_ACTIVATE => VmnetTopologyMessage::Activate {
            context,
            session,
            authority: decode_authority(encoded)?,
        },
        KIND_BROKER_READY => VmnetTopologyMessage::BrokerReady { context, session },
        KIND_LAUNCHER_READY => VmnetTopologyMessage::LauncherReady { context, session },
        KIND_READY_ACK => VmnetTopologyMessage::ReadyAck { context, session },
        KIND_CANCEL => VmnetTopologyMessage::Cancel {
            context,
            session,
            reason: VmnetTopologyCancelReason::decode(
                *encoded.get(103).ok_or(VmnetTopologyError)?,
            )?,
        },
        KIND_TERMINAL => VmnetTopologyMessage::Terminal {
            context,
            session,
            result: VmnetTopologyTerminal::decode(*encoded.get(103).ok_or(VmnetTopologyError)?)?,
        },
        KIND_TERMINAL_ACK => VmnetTopologyMessage::TerminalAck {
            context,
            session,
            result: VmnetTopologyTerminal::decode(*encoded.get(103).ok_or(VmnetTopologyError)?)?,
        },
        _ => return Err(VmnetTopologyError),
    }
    .validated()?;
    if read_u64(encoded, 16)? != frame.sequence()
        || encode_vmnet_topology_message(frame)?.as_slice() != encoded
    {
        return Err(VmnetTopologyError);
    }
    Ok(frame)
}

fn kind(message: VmnetTopologyMessage) -> u8 {
    match message {
        VmnetTopologyMessage::Start(_) => KIND_START,
        VmnetTopologyMessage::Dropped(_) => KIND_DROPPED,
        VmnetTopologyMessage::DropAck(_) => KIND_DROP_ACK,
        VmnetTopologyMessage::OuterStart(_) => KIND_OUTER_START,
        VmnetTopologyMessage::OuterHello(_) => KIND_OUTER_HELLO,
        VmnetTopologyMessage::Proceed(_) => KIND_PROCEED,
        VmnetTopologyMessage::Activate { .. } => KIND_ACTIVATE,
        VmnetTopologyMessage::BrokerReady { .. } => KIND_BROKER_READY,
        VmnetTopologyMessage::LauncherReady { .. } => KIND_LAUNCHER_READY,
        VmnetTopologyMessage::ReadyAck { .. } => KIND_READY_ACK,
        VmnetTopologyMessage::Cancel { .. } => KIND_CANCEL,
        VmnetTopologyMessage::Terminal { .. } => KIND_TERMINAL,
        VmnetTopologyMessage::TerminalAck { .. } => KIND_TERMINAL_ACK,
    }
}

fn encode_authority(
    encoded: &mut [u8; VMNET_TOPOLOGY_FRAME_BYTES],
    authority: VmnetAuthority,
) -> Result<(), VmnetTopologyError> {
    if authority.is_denied() {
        return Err(VmnetTopologyError);
    }
    encoded[100] = (if authority.allows_host() {
        FLAG_HOST
    } else {
        0
    }) | (if authority.allows_shared() {
        FLAG_SHARED
    } else {
        0
    });
    encoded[101] = authority.max_interfaces().ok_or(VmnetTopologyError)?;
    let bridge_count = (0..MAX_VMNET_BRIDGE_NAMES)
        .take_while(|index| authority.bridge_slot(*index).is_some())
        .count();
    encoded[102] = u8::try_from(bridge_count).map_err(|_| VmnetTopologyError)?;
    for index in 0..bridge_count {
        let name = authority.bridge_slot(index).ok_or(VmnetTopologyError)?;
        let offset = 104 + index * BRIDGE_SLOT_BYTES;
        *encoded.get_mut(offset).ok_or(VmnetTopologyError)? =
            u8::try_from(name.len()).map_err(|_| VmnetTopologyError)?;
        encoded
            .get_mut(offset + 1..offset + 1 + name.len())
            .ok_or(VmnetTopologyError)?
            .copy_from_slice(name.as_bytes());
    }
    Ok(())
}

fn decode_authority(encoded: &[u8]) -> Result<VmnetAuthority, VmnetTopologyError> {
    let flags = *encoded.get(100).ok_or(VmnetTopologyError)?;
    if flags & !AUTHORITY_FLAGS != 0 || *encoded.get(103).ok_or(VmnetTopologyError)? != 0 {
        return Err(VmnetTopologyError);
    }
    let bridge_count = usize::from(*encoded.get(102).ok_or(VmnetTopologyError)?);
    if bridge_count > MAX_VMNET_BRIDGE_NAMES {
        return Err(VmnetTopologyError);
    }
    let mut names = [""; MAX_VMNET_BRIDGE_NAMES];
    for (index, name) in names.iter_mut().enumerate() {
        let offset = 104 + index * BRIDGE_SLOT_BYTES;
        let length = usize::from(*encoded.get(offset).ok_or(VmnetTopologyError)?);
        let slot = encoded
            .get(offset + 1..offset + BRIDGE_SLOT_BYTES)
            .ok_or(VmnetTopologyError)?;
        if index < bridge_count {
            if length == 0
                || length > MAX_VMNET_BRIDGE_NAME_BYTES
                || slot
                    .get(length..)
                    .is_none_or(|padding| padding.iter().any(|byte| *byte != 0))
            {
                return Err(VmnetTopologyError);
            }
            *name = std::str::from_utf8(slot.get(..length).ok_or(VmnetTopologyError)?)
                .map_err(|_| VmnetTopologyError)?;
        } else if length != 0 || slot != [0; MAX_VMNET_BRIDGE_NAME_BYTES] {
            return Err(VmnetTopologyError);
        }
    }
    VmnetAuthority::try_new(
        flags & FLAG_HOST != 0,
        flags & FLAG_SHARED != 0,
        *encoded.get(101).ok_or(VmnetTopologyError)?,
        names.get(..bridge_count).ok_or(VmnetTopologyError)?,
    )
    .map_err(|_| VmnetTopologyError)
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, VmnetTopologyError> {
    bytes
        .get(offset..offset + 2)
        .ok_or(VmnetTopologyError)?
        .try_into()
        .map(u16::from_be_bytes)
        .map_err(|_| VmnetTopologyError)
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, VmnetTopologyError> {
    bytes
        .get(offset..offset + 4)
        .ok_or(VmnetTopologyError)?
        .try_into()
        .map(u32::from_be_bytes)
        .map_err(|_| VmnetTopologyError)
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, VmnetTopologyError> {
    bytes
        .get(offset..offset + 8)
        .ok_or(VmnetTopologyError)?
        .try_into()
        .map(u64::from_be_bytes)
        .map_err(|_| VmnetTopologyError)
}

fn read_array<const N: usize>(bytes: &[u8], offset: usize) -> Result<[u8; N], VmnetTopologyError> {
    bytes
        .get(offset..offset + N)
        .ok_or(VmnetTopologyError)?
        .try_into()
        .map_err(|_| VmnetTopologyError)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> VmnetTopologyContext {
        VmnetTopologyContext::new(
            SessionId::from_bytes([3; 32]),
            CredentialTarget::new(501, 20).expect("target should validate"),
            42,
            VmnetTopologyMode::Foreground,
        )
        .expect("context should validate")
    }

    fn session() -> SessionId {
        SessionId::from_bytes([7; 32])
    }

    fn authority() -> VmnetAuthority {
        VmnetAuthority::try_new(true, true, 4, &["en0", "bridge_1"])
            .expect("authority should validate")
    }

    fn messages() -> Vec<VmnetTopologyMessage> {
        vec![
            VmnetTopologyMessage::Start(context()),
            VmnetTopologyMessage::Dropped(context()),
            VmnetTopologyMessage::DropAck(context()),
            VmnetTopologyMessage::OuterStart(context()),
            VmnetTopologyMessage::OuterHello(context()),
            VmnetTopologyMessage::Proceed(context()),
            VmnetTopologyMessage::Activate {
                context: context(),
                session: session(),
                authority: authority(),
            },
            VmnetTopologyMessage::BrokerReady {
                context: context(),
                session: session(),
            },
            VmnetTopologyMessage::LauncherReady {
                context: context(),
                session: session(),
            },
            VmnetTopologyMessage::ReadyAck {
                context: context(),
                session: session(),
            },
            VmnetTopologyMessage::Cancel {
                context: context(),
                session: session(),
                reason: VmnetTopologyCancelReason::Signal,
            },
            VmnetTopologyMessage::Terminal {
                context: context(),
                session: session(),
                result: VmnetTopologyTerminal::Complete,
            },
            VmnetTopologyMessage::TerminalAck {
                context: context(),
                session: session(),
                result: VmnetTopologyTerminal::Complete,
            },
        ]
    }

    #[test]
    fn every_message_round_trips_canonically_and_is_redacted() {
        for message in messages() {
            let encoded = encode_vmnet_topology_message(message).expect("message should encode");
            assert_eq!(decode_vmnet_topology_message(&encoded), Ok(message));
            assert_eq!(format!("{message:?}"), "VmnetTopologyMessage(<redacted>)");
        }
    }

    #[test]
    fn rejects_root_pre_session_zero_pid_and_denied_activation() {
        assert!(
            VmnetTopologyContext::new(
                SessionId::pre_session(),
                context().target(),
                1,
                VmnetTopologyMode::Foreground
            )
            .is_err()
        );
        assert!(
            VmnetTopologyContext::new(
                context().correlation(),
                CredentialTarget::new(0, 0).expect("root target"),
                1,
                VmnetTopologyMode::Foreground
            )
            .is_err()
        );
        assert!(
            VmnetTopologyContext::new(
                context().correlation(),
                context().target(),
                0,
                VmnetTopologyMode::Foreground
            )
            .is_err()
        );
        assert!(
            encode_vmnet_topology_message(VmnetTopologyMessage::Activate {
                context: context(),
                session: session(),
                authority: VmnetAuthority::denied(),
            })
            .is_err()
        );
    }

    #[test]
    fn rejects_every_header_body_sequence_and_padding_class() {
        let encoded = encode_vmnet_topology_message(VmnetTopologyMessage::Activate {
            context: context(),
            session: session(),
            authority: authority(),
        })
        .expect("activation should encode");
        for offset in [0, 9, 10, 11, 12, 23, 100, 101, 102, 103, 104, 168] {
            let mut damaged = encoded;
            damaged[offset] ^= 0xff;
            assert_eq!(
                decode_vmnet_topology_message(&damaged),
                Err(VmnetTopologyError),
                "offset {offset} should reject"
            );
        }
        for range in [24..56, 56..88, 88..96, 96..100] {
            let mut damaged = encoded;
            damaged[range.clone()].fill(0);
            assert_eq!(
                decode_vmnet_topology_message(&damaged),
                Err(VmnetTopologyError),
                "range {range:?} should reject"
            );
        }
        assert_eq!(
            decode_vmnet_topology_message(&encoded[..VMNET_TOPOLOGY_FRAME_BYTES - 1]),
            Err(VmnetTopologyError)
        );
    }

    #[test]
    fn exact_sequence_is_closed() {
        for (index, message) in messages().into_iter().take(10).enumerate() {
            assert_eq!(
                message.sequence(),
                u64::try_from(index).expect("index should fit")
            );
        }
    }

    #[test]
    fn transport_normalizes_an_inherited_nonblocking_stream() {
        let (inherited, peer) = UnixStream::pair().expect("stream pair should open");
        inherited
            .set_nonblocking(true)
            .expect("inherited stream should become nonblocking");
        let mut receiver = VmnetTopologyTransport::new(inherited, Duration::from_secs(1))
            .expect("transport should adopt the stream");
        // SAFETY: The receiver owns a live descriptor for this synchronous
        // status-flag inspection.
        let flags = unsafe { libc::fcntl(receiver.as_raw_fd(), libc::F_GETFL) };
        assert!(flags >= 0);
        assert_eq!(flags & libc::O_NONBLOCK, 0);

        let mut sender = VmnetTopologyTransport::new(peer, Duration::from_secs(1))
            .expect("peer transport should initialize");
        let message = VmnetTopologyMessage::Start(context());
        sender.send(message).expect("message should send");
        assert_eq!(receiver.receive(), Ok(message));
    }
}
