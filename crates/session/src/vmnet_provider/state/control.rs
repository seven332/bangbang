use std::collections::BTreeMap;
use std::fmt;
use std::os::unix::net::UnixStream;

use crate::SessionId;

use super::{first_sequence, require_session};
use crate::vmnet_provider::{
    ControlMessage, MAX_ACTIVE_INTERFACES, ProviderCancelReason, ProviderCleanup, ProviderEnvelope,
    ProviderFrame, ProviderStatus, ProviderTerminalCode, RealizedVmnetParameters,
    RequestedVmnetParameters, VmnetGeneration, VmnetInterfaceId, VmnetPolicySlot,
    VmnetProviderError, VmnetSequence,
};

/// Worker-side control lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlClientState {
    /// No hello has been sent.
    New,
    /// The hello acknowledgement is pending.
    AwaitHelloAck,
    /// The client may start, stop, cancel, or shut down.
    Ready,
    /// One start result is pending.
    AwaitStart,
    /// One stop result is pending.
    AwaitStop,
    /// Cancellation acknowledgement, and possibly one raced result, is pending.
    AwaitCancelled,
    /// Orderly shutdown acknowledgement is pending.
    AwaitShutdownAck,
    /// The protocol completed cleanly.
    Closed,
    /// A terminal failure permanently poisoned the state.
    Terminal,
}

/// Broker-side control lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlBrokerState {
    /// The client hello is pending.
    AwaitHello,
    /// A hello acknowledgement must be emitted.
    ReadyToAck,
    /// The broker may accept a new operation.
    Ready,
    /// The adapter owns one pending start.
    StartPending,
    /// The adapter owns one pending stop.
    StopPending,
    /// Cancellation cleanup and acknowledgement are required.
    ReadyToCancel,
    /// An orderly shutdown acknowledgement is required.
    ReadyToShutdown,
    /// The protocol completed cleanly.
    Closed,
    /// A terminal failure permanently poisoned the state.
    Terminal,
}

/// Checked event produced by the worker-side control state.
pub enum ControlClientEvent {
    /// The broker accepted the session binding.
    Ready,
    /// One interface started and owns its transferred data stream.
    Started {
        /// Exact interface identity.
        interface: VmnetInterfaceId,
        /// Fresh interface generation.
        generation: VmnetGeneration,
        /// Validated backend parameters.
        parameters: RealizedVmnetParameters,
        /// Atomically accepted data stream.
        stream: UnixStream,
    },
    /// One start failed without creating ownership.
    StartFailed {
        /// Exact interface identity.
        interface: VmnetInterfaceId,
        /// Stable failure category.
        status: ProviderStatus,
    },
    /// One active generation stopped completely.
    Stopped {
        /// Exact interface identity.
        interface: VmnetInterfaceId,
        /// Retired generation.
        generation: VmnetGeneration,
    },
    /// A raced start was accepted and its stream retired internally.
    StartRetiredDuringCancellation {
        /// Exact interface identity.
        interface: VmnetInterfaceId,
        /// Raced generation.
        generation: VmnetGeneration,
    },
    /// A failed start was serialized before cancellation.
    StartFailedDuringCancellation {
        /// Exact interface identity.
        interface: VmnetInterfaceId,
        /// Stable failure category.
        status: ProviderStatus,
    },
    /// A stop was serialized before cancellation.
    StoppedDuringCancellation {
        /// Exact interface identity.
        interface: VmnetInterfaceId,
        /// Retired generation.
        generation: VmnetGeneration,
    },
    /// Cancellation retired all session ownership.
    Cancelled,
    /// Orderly shutdown completed.
    Shutdown,
    /// The peer declared a terminal category.
    PeerTerminal {
        /// Stable terminal category.
        code: ProviderTerminalCode,
    },
}

impl fmt::Debug for ControlClientEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ready => formatter.write_str("Ready"),
            Self::Started { .. } => formatter.write_str("Started(<redacted>)"),
            Self::StartFailed { status, .. } => formatter
                .debug_struct("StartFailed")
                .field("interface", &"<redacted>")
                .field("status", status)
                .finish(),
            Self::Stopped { .. } => formatter.write_str("Stopped(<redacted>)"),
            Self::StartRetiredDuringCancellation { .. } => {
                formatter.write_str("StartRetiredDuringCancellation(<redacted>)")
            }
            Self::StartFailedDuringCancellation { status, .. } => formatter
                .debug_struct("StartFailedDuringCancellation")
                .field("interface", &"<redacted>")
                .field("status", status)
                .finish(),
            Self::StoppedDuringCancellation { .. } => {
                formatter.write_str("StoppedDuringCancellation(<redacted>)")
            }
            Self::Cancelled => formatter.write_str("Cancelled"),
            Self::Shutdown => formatter.write_str("Shutdown"),
            Self::PeerTerminal { code } => formatter
                .debug_struct("PeerTerminal")
                .field("code", code)
                .finish(),
        }
    }
}

/// Checked event produced by the broker-side control state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlBrokerEvent {
    /// A session hello was accepted.
    Hello,
    /// A typed start requires backend work.
    Start {
        /// Exact interface identity.
        interface: VmnetInterfaceId,
        /// Bootstrap-owned fixed policy slot.
        policy_slot: VmnetPolicySlot,
        /// Validated optional parameters.
        requested: RequestedVmnetParameters,
    },
    /// An exact active generation requires stop and reap.
    Stop {
        /// Exact interface identity.
        interface: VmnetInterfaceId,
        /// Exact active generation.
        generation: VmnetGeneration,
    },
    /// Session cancellation requires aggregate cleanup.
    Cancel {
        /// Stable cancellation source.
        reason: ProviderCancelReason,
    },
    /// Empty-session orderly shutdown was requested.
    Shutdown,
    /// The peer declared a terminal category.
    PeerTerminal {
        /// Stable terminal category.
        code: ProviderTerminalCode,
    },
}

#[derive(Debug, Clone, Copy)]
enum ControlPending {
    Start(VmnetInterfaceId),
    Stop(VmnetInterfaceId, VmnetGeneration),
}

/// Worker-side session-bound vmnet control state machine.
pub struct VmnetControlClient {
    session: SessionId,
    state: ControlClientState,
    next_local: VmnetSequence,
    next_peer: VmnetSequence,
    pending: Option<ControlPending>,
    active: BTreeMap<VmnetInterfaceId, VmnetGeneration>,
    last_generation: Option<VmnetGeneration>,
}

impl VmnetControlClient {
    /// Constructs a control client for one established lifecycle session.
    pub fn new(session: SessionId) -> Result<Self, VmnetProviderError> {
        require_session(session)?;
        Ok(Self {
            session,
            state: ControlClientState::New,
            next_local: first_sequence(),
            next_peer: first_sequence(),
            pending: None,
            active: BTreeMap::new(),
            last_generation: None,
        })
    }

    /// Returns the current lifecycle without exposing ownership values.
    #[must_use]
    pub const fn state(&self) -> ControlClientState {
        self.state
    }

    /// Returns the bounded active-interface count.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Returns the owned generation for an interface.
    #[must_use]
    pub fn active_generation(&self, interface: VmnetInterfaceId) -> Option<VmnetGeneration> {
        self.active.get(&interface).copied()
    }

    /// Begins the mandatory control handshake.
    pub fn hello(&mut self) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(ControlClientState::New)?;
        let envelope = self.control(None, None, ControlMessage::Hello)?;
        self.state = ControlClientState::AwaitHelloAck;
        Ok(envelope)
    }

    /// Begins one typed interface start.
    pub fn start(
        &mut self,
        interface: VmnetInterfaceId,
        policy_slot: VmnetPolicySlot,
        requested: RequestedVmnetParameters,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(ControlClientState::Ready)?;
        if self.active.len() >= MAX_ACTIVE_INTERFACES || self.active.contains_key(&interface) {
            return Err(VmnetProviderError::LimitExceeded);
        }
        let envelope = self.control(
            Some(interface),
            None,
            ControlMessage::Start {
                policy_slot,
                requested,
            },
        )?;
        self.pending = Some(ControlPending::Start(interface));
        self.state = ControlClientState::AwaitStart;
        Ok(envelope)
    }

    /// Begins exact stop of one active generation.
    pub fn stop(
        &mut self,
        interface: VmnetInterfaceId,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(ControlClientState::Ready)?;
        let generation = self
            .active
            .get(&interface)
            .copied()
            .ok_or(VmnetProviderError::InvalidLifecycle)?;
        let envelope = self.control(Some(interface), Some(generation), ControlMessage::Stop)?;
        self.pending = Some(ControlPending::Stop(interface, generation));
        self.state = ControlClientState::AwaitStop;
        Ok(envelope)
    }

    /// Cancels every pending and active interface in an established session.
    pub fn cancel(
        &mut self,
        reason: ProviderCancelReason,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        if !matches!(
            self.state,
            ControlClientState::Ready
                | ControlClientState::AwaitStart
                | ControlClientState::AwaitStop
        ) {
            return Err(self.local_error());
        }
        let envelope = self.control(None, None, ControlMessage::Cancel { reason })?;
        self.state = ControlClientState::AwaitCancelled;
        Ok(envelope)
    }

    /// Begins orderly shutdown after all ownership is retired.
    pub fn shutdown(&mut self) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(ControlClientState::Ready)?;
        if !self.active.is_empty() || self.pending.is_some() {
            return Err(VmnetProviderError::InvalidLifecycle);
        }
        let envelope = self.control(None, None, ControlMessage::Shutdown)?;
        self.state = ControlClientState::AwaitShutdownAck;
        Ok(envelope)
    }

    /// Emits a terminal frame and irreversibly drops local ownership state.
    pub fn terminal(
        &mut self,
        code: ProviderTerminalCode,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        if matches!(
            self.state,
            ControlClientState::Closed | ControlClientState::Terminal
        ) {
            return Err(self.local_error());
        }
        let envelope = self.control(None, None, ControlMessage::Terminal { code })?;
        self.poison();
        Ok(envelope)
    }

    /// Consumes one atomically received broker frame and optional stream.
    pub fn receive(
        &mut self,
        envelope: ProviderEnvelope,
    ) -> Result<ControlClientEvent, VmnetProviderError> {
        if matches!(
            self.state,
            ControlClientState::Closed | ControlClientState::Terminal
        ) {
            return Err(self.local_error());
        }
        let (frame, stream) = envelope.into_parts();
        if self.validate_peer(&frame).is_err()
            || usize::from(frame.descriptor_count()) != usize::from(stream.is_some())
        {
            return Err(self.peer_error());
        }
        let message = frame.control_message().cloned();
        let result = (|| match (self.state, message) {
            (ControlClientState::AwaitHelloAck, Some(ControlMessage::HelloAck)) => {
                self.state = ControlClientState::Ready;
                Ok(ControlClientEvent::Ready)
            }
            (ControlClientState::AwaitStart, Some(ControlMessage::Started { parameters })) => {
                let ControlPending::Start(interface) =
                    self.pending.ok_or(VmnetProviderError::InvalidPeerState)?
                else {
                    return Err(self.peer_error());
                };
                self.accept_started(&frame, interface)?;
                let generation = frame
                    .generation()
                    .ok_or(VmnetProviderError::InvalidPeerState)?;
                let stream = stream.ok_or(VmnetProviderError::InvalidPeerState)?;
                self.active.insert(interface, generation);
                self.last_generation = Some(generation);
                self.pending = None;
                self.state = ControlClientState::Ready;
                Ok(ControlClientEvent::Started {
                    interface,
                    generation,
                    parameters,
                    stream,
                })
            }
            (ControlClientState::AwaitStart, Some(ControlMessage::StartFailed { status })) => {
                let ControlPending::Start(interface) =
                    self.pending.ok_or(VmnetProviderError::InvalidPeerState)?
                else {
                    return Err(self.peer_error());
                };
                if frame.interface() != Some(interface) || frame.generation().is_some() {
                    Err(VmnetProviderError::InvalidPeerState)
                } else {
                    self.pending = None;
                    self.state = ControlClientState::Ready;
                    Ok(ControlClientEvent::StartFailed { interface, status })
                }
            }
            (ControlClientState::AwaitStop, Some(ControlMessage::Stopped { cleanup })) => {
                let ControlPending::Stop(interface, generation) =
                    self.pending.ok_or(VmnetProviderError::InvalidPeerState)?
                else {
                    return Err(self.peer_error());
                };
                self.accept_stopped(&frame, interface, generation, cleanup)?;
                self.active.remove(&interface);
                self.pending = None;
                self.state = ControlClientState::Ready;
                Ok(ControlClientEvent::Stopped {
                    interface,
                    generation,
                })
            }
            (ControlClientState::AwaitCancelled, Some(message)) => {
                self.receive_during_cancellation(&frame, stream, message)
            }
            (ControlClientState::AwaitShutdownAck, Some(ControlMessage::ShutdownAck)) => {
                self.state = ControlClientState::Closed;
                Ok(ControlClientEvent::Shutdown)
            }
            (_, Some(ControlMessage::Terminal { code })) => {
                self.poison();
                Ok(ControlClientEvent::PeerTerminal { code })
            }
            _ => Err(VmnetProviderError::InvalidPeerState),
        })();
        if result.is_err() {
            self.poison();
        }
        result
    }
}

impl VmnetControlClient {
    fn receive_during_cancellation(
        &mut self,
        frame: &ProviderFrame,
        stream: Option<UnixStream>,
        message: ControlMessage,
    ) -> Result<ControlClientEvent, VmnetProviderError> {
        match (self.pending, message) {
            (Some(ControlPending::Start(interface)), ControlMessage::Started { .. }) => {
                self.accept_started(frame, interface)?;
                let generation = frame
                    .generation()
                    .ok_or(VmnetProviderError::InvalidPeerState)?;
                let _retired_stream = stream.ok_or(VmnetProviderError::InvalidPeerState)?;
                self.last_generation = Some(generation);
                self.pending = None;
                Ok(ControlClientEvent::StartRetiredDuringCancellation {
                    interface,
                    generation,
                })
            }
            (Some(ControlPending::Start(interface)), ControlMessage::StartFailed { status })
                if frame.interface() == Some(interface) && frame.generation().is_none() =>
            {
                self.pending = None;
                Ok(ControlClientEvent::StartFailedDuringCancellation { interface, status })
            }
            (
                Some(ControlPending::Stop(interface, generation)),
                ControlMessage::Stopped {
                    cleanup: ProviderCleanup::Complete,
                },
            ) if frame.interface() == Some(interface) && frame.generation() == Some(generation) => {
                self.active.remove(&interface);
                self.pending = None;
                Ok(ControlClientEvent::StoppedDuringCancellation {
                    interface,
                    generation,
                })
            }
            (
                _,
                ControlMessage::Cancelled {
                    cleanup: ProviderCleanup::Complete,
                },
            ) => {
                self.pending = None;
                self.active.clear();
                self.state = ControlClientState::Closed;
                Ok(ControlClientEvent::Cancelled)
            }
            _ => Err(VmnetProviderError::InvalidPeerState),
        }
    }

    fn accept_started(
        &self,
        frame: &ProviderFrame,
        interface: VmnetInterfaceId,
    ) -> Result<(), VmnetProviderError> {
        let generation = frame
            .generation()
            .ok_or(VmnetProviderError::InvalidPeerState)?;
        if frame.interface() != Some(interface)
            || self.active.contains_key(&interface)
            || self.last_generation.is_some_and(|last| generation <= last)
        {
            return Err(VmnetProviderError::InvalidPeerState);
        }
        Ok(())
    }

    fn accept_stopped(
        &self,
        frame: &ProviderFrame,
        interface: VmnetInterfaceId,
        generation: VmnetGeneration,
        cleanup: ProviderCleanup,
    ) -> Result<(), VmnetProviderError> {
        if frame.interface() != Some(interface)
            || frame.generation() != Some(generation)
            || cleanup != ProviderCleanup::Complete
        {
            return Err(VmnetProviderError::InvalidPeerState);
        }
        Ok(())
    }

    fn control(
        &mut self,
        interface: Option<VmnetInterfaceId>,
        generation: Option<VmnetGeneration>,
        message: ControlMessage,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        let sequence = self.take_local_sequence()?;
        ProviderFrame::control(self.session, interface, generation, sequence, message)
            .map(ProviderEnvelope::frame_only)
    }

    fn validate_peer(&mut self, frame: &ProviderFrame) -> Result<(), VmnetProviderError> {
        if frame.session() != self.session
            || frame.sequence() != self.next_peer
            || frame.control_message().is_none()
        {
            return Err(VmnetProviderError::InvalidPeerState);
        }
        self.next_peer = self.next_peer.checked_next()?;
        Ok(())
    }

    fn take_local_sequence(&mut self) -> Result<VmnetSequence, VmnetProviderError> {
        let current = self.next_local;
        match self.next_local.checked_next() {
            Ok(next) => {
                self.next_local = next;
                Ok(current)
            }
            Err(error) => {
                self.poison();
                Err(error)
            }
        }
    }

    fn require_state(&self, expected: ControlClientState) -> Result<(), VmnetProviderError> {
        if self.state == expected {
            Ok(())
        } else {
            Err(self.local_error())
        }
    }

    fn local_error(&self) -> VmnetProviderError {
        if self.state == ControlClientState::Terminal {
            VmnetProviderError::Poisoned
        } else {
            VmnetProviderError::InvalidLifecycle
        }
    }

    fn peer_error(&mut self) -> VmnetProviderError {
        self.poison();
        VmnetProviderError::InvalidPeerState
    }

    fn poison(&mut self) {
        self.pending = None;
        self.active.clear();
        self.state = ControlClientState::Terminal;
    }
}

impl fmt::Debug for VmnetControlClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetControlClient")
            .field("session", &"<redacted>")
            .field("state", &self.state)
            .field("active_count", &self.active.len())
            .finish()
    }
}

/// Bootstrap-side session-bound vmnet control state machine.
pub struct VmnetControlBroker {
    session: SessionId,
    state: ControlBrokerState,
    next_local: VmnetSequence,
    next_peer: VmnetSequence,
    pending: Option<ControlPending>,
    active: BTreeMap<VmnetInterfaceId, VmnetGeneration>,
    last_generation: Option<VmnetGeneration>,
}

impl VmnetControlBroker {
    /// Constructs a broker for one established lifecycle session.
    pub fn new(session: SessionId) -> Result<Self, VmnetProviderError> {
        require_session(session)?;
        Ok(Self {
            session,
            state: ControlBrokerState::AwaitHello,
            next_local: first_sequence(),
            next_peer: first_sequence(),
            pending: None,
            active: BTreeMap::new(),
            last_generation: None,
        })
    }

    /// Returns the current lifecycle.
    #[must_use]
    pub const fn state(&self) -> ControlBrokerState {
        self.state
    }

    /// Returns the bounded active-interface count.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.active.len()
    }

    /// Consumes one checked client control frame.
    pub fn receive(
        &mut self,
        envelope: ProviderEnvelope,
    ) -> Result<ControlBrokerEvent, VmnetProviderError> {
        if matches!(
            self.state,
            ControlBrokerState::Closed | ControlBrokerState::Terminal
        ) {
            return Err(self.local_error());
        }
        let (frame, stream) = envelope.into_parts();
        if stream.is_some() || self.validate_peer(&frame).is_err() {
            return Err(self.peer_error());
        }
        let message = frame.control_message().cloned();
        let result = (|| match (self.state, message) {
            (ControlBrokerState::AwaitHello, Some(ControlMessage::Hello)) => {
                self.state = ControlBrokerState::ReadyToAck;
                Ok(ControlBrokerEvent::Hello)
            }
            (
                ControlBrokerState::Ready,
                Some(ControlMessage::Start {
                    policy_slot,
                    requested,
                }),
            ) => {
                let interface = frame
                    .interface()
                    .ok_or(VmnetProviderError::InvalidPeerState)?;
                if self.active.len() >= MAX_ACTIVE_INTERFACES
                    || self.active.contains_key(&interface)
                {
                    Err(VmnetProviderError::InvalidPeerState)
                } else {
                    self.pending = Some(ControlPending::Start(interface));
                    self.state = ControlBrokerState::StartPending;
                    Ok(ControlBrokerEvent::Start {
                        interface,
                        policy_slot,
                        requested,
                    })
                }
            }
            (ControlBrokerState::Ready, Some(ControlMessage::Stop)) => {
                let interface = frame
                    .interface()
                    .ok_or(VmnetProviderError::InvalidPeerState)?;
                let generation = frame
                    .generation()
                    .ok_or(VmnetProviderError::InvalidPeerState)?;
                if self.active.get(&interface) != Some(&generation) {
                    Err(VmnetProviderError::InvalidPeerState)
                } else {
                    self.pending = Some(ControlPending::Stop(interface, generation));
                    self.state = ControlBrokerState::StopPending;
                    Ok(ControlBrokerEvent::Stop {
                        interface,
                        generation,
                    })
                }
            }
            (
                ControlBrokerState::Ready
                | ControlBrokerState::StartPending
                | ControlBrokerState::StopPending,
                Some(ControlMessage::Cancel { reason }),
            ) => {
                self.state = ControlBrokerState::ReadyToCancel;
                Ok(ControlBrokerEvent::Cancel { reason })
            }
            (ControlBrokerState::Ready, Some(ControlMessage::Shutdown))
                if self.active.is_empty() && self.pending.is_none() =>
            {
                self.state = ControlBrokerState::ReadyToShutdown;
                Ok(ControlBrokerEvent::Shutdown)
            }
            (_, Some(ControlMessage::Terminal { code })) => {
                self.poison();
                Ok(ControlBrokerEvent::PeerTerminal { code })
            }
            _ => Err(VmnetProviderError::InvalidPeerState),
        })();
        if result.is_err() {
            self.poison();
        }
        result
    }

    /// Emits the mandatory hello acknowledgement.
    pub fn hello_ack(&mut self) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(ControlBrokerState::ReadyToAck)?;
        let envelope = self.control(None, None, ControlMessage::HelloAck)?;
        self.state = ControlBrokerState::Ready;
        Ok(envelope)
    }

    /// Commits a fresh start and atomically transfers its data stream.
    pub fn started(
        &mut self,
        generation: VmnetGeneration,
        parameters: RealizedVmnetParameters,
        stream: UnixStream,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        if !matches!(
            self.state,
            ControlBrokerState::StartPending | ControlBrokerState::ReadyToCancel
        ) {
            return Err(self.local_error());
        }
        let ControlPending::Start(interface) =
            self.pending.ok_or(VmnetProviderError::InvalidLifecycle)?
        else {
            return Err(VmnetProviderError::InvalidLifecycle);
        };
        if self.last_generation.is_some_and(|last| generation <= last) {
            return Err(VmnetProviderError::InvalidConfiguration);
        }
        let sequence = self.take_local_sequence()?;
        let frame = ProviderFrame::control(
            self.session,
            Some(interface),
            Some(generation),
            sequence,
            ControlMessage::Started { parameters },
        )?;
        self.active.insert(interface, generation);
        self.last_generation = Some(generation);
        self.pending = None;
        if self.state == ControlBrokerState::StartPending {
            self.state = ControlBrokerState::Ready;
        }
        Ok(ProviderEnvelope::with_stream(frame, stream))
    }

    /// Completes a pending start without creating ownership.
    pub fn start_failed(
        &mut self,
        status: ProviderStatus,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        if !matches!(
            self.state,
            ControlBrokerState::StartPending | ControlBrokerState::ReadyToCancel
        ) {
            return Err(self.local_error());
        }
        let ControlPending::Start(interface) =
            self.pending.ok_or(VmnetProviderError::InvalidLifecycle)?
        else {
            return Err(VmnetProviderError::InvalidLifecycle);
        };
        let envelope = self.control(
            Some(interface),
            None,
            ControlMessage::StartFailed { status },
        )?;
        self.pending = None;
        if self.state == ControlBrokerState::StartPending {
            self.state = ControlBrokerState::Ready;
        }
        Ok(envelope)
    }

    /// Completes exact stop and reap of a pending generation.
    pub fn stopped(
        &mut self,
        cleanup: ProviderCleanup,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        if !matches!(
            self.state,
            ControlBrokerState::StopPending | ControlBrokerState::ReadyToCancel
        ) {
            return Err(self.local_error());
        }
        let ControlPending::Stop(interface, generation) =
            self.pending.ok_or(VmnetProviderError::InvalidLifecycle)?
        else {
            return Err(VmnetProviderError::InvalidLifecycle);
        };
        let envelope = self.control(
            Some(interface),
            Some(generation),
            ControlMessage::Stopped { cleanup },
        )?;
        self.pending = None;
        if cleanup == ProviderCleanup::Complete {
            self.active.remove(&interface);
            if self.state == ControlBrokerState::StopPending {
                self.state = ControlBrokerState::Ready;
            }
        } else {
            self.poison();
        }
        Ok(envelope)
    }

    /// Completes aggregate cancellation, suppressing any still-pending result.
    pub fn cancelled(
        &mut self,
        cleanup: ProviderCleanup,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(ControlBrokerState::ReadyToCancel)?;
        let envelope = self.control(None, None, ControlMessage::Cancelled { cleanup })?;
        self.pending = None;
        self.active.clear();
        self.state = if cleanup == ProviderCleanup::Complete {
            ControlBrokerState::Closed
        } else {
            ControlBrokerState::Terminal
        };
        Ok(envelope)
    }

    /// Completes empty-session orderly shutdown.
    pub fn shutdown_ack(&mut self) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(ControlBrokerState::ReadyToShutdown)?;
        let envelope = self.control(None, None, ControlMessage::ShutdownAck)?;
        self.state = ControlBrokerState::Closed;
        Ok(envelope)
    }

    /// Emits a terminal frame and irreversibly drops broker ownership state.
    pub fn terminal(
        &mut self,
        code: ProviderTerminalCode,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        if matches!(
            self.state,
            ControlBrokerState::Closed | ControlBrokerState::Terminal
        ) {
            return Err(self.local_error());
        }
        let envelope = self.control(None, None, ControlMessage::Terminal { code })?;
        self.poison();
        Ok(envelope)
    }
}

impl VmnetControlBroker {
    fn control(
        &mut self,
        interface: Option<VmnetInterfaceId>,
        generation: Option<VmnetGeneration>,
        message: ControlMessage,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        let sequence = self.take_local_sequence()?;
        ProviderFrame::control(self.session, interface, generation, sequence, message)
            .map(ProviderEnvelope::frame_only)
    }

    fn validate_peer(&mut self, frame: &ProviderFrame) -> Result<(), VmnetProviderError> {
        if frame.session() != self.session
            || frame.sequence() != self.next_peer
            || frame.control_message().is_none()
            || frame.descriptor_count() != 0
        {
            return Err(VmnetProviderError::InvalidPeerState);
        }
        self.next_peer = self.next_peer.checked_next()?;
        Ok(())
    }

    fn take_local_sequence(&mut self) -> Result<VmnetSequence, VmnetProviderError> {
        let current = self.next_local;
        match self.next_local.checked_next() {
            Ok(next) => {
                self.next_local = next;
                Ok(current)
            }
            Err(error) => {
                self.poison();
                Err(error)
            }
        }
    }

    fn require_state(&self, expected: ControlBrokerState) -> Result<(), VmnetProviderError> {
        if self.state == expected {
            Ok(())
        } else {
            Err(self.local_error())
        }
    }

    fn local_error(&self) -> VmnetProviderError {
        if self.state == ControlBrokerState::Terminal {
            VmnetProviderError::Poisoned
        } else {
            VmnetProviderError::InvalidLifecycle
        }
    }

    fn peer_error(&mut self) -> VmnetProviderError {
        self.poison();
        VmnetProviderError::InvalidPeerState
    }

    fn poison(&mut self) {
        self.pending = None;
        self.active.clear();
        self.state = ControlBrokerState::Terminal;
    }
}

impl fmt::Debug for VmnetControlBroker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetControlBroker")
            .field("session", &"<redacted>")
            .field("state", &self.state)
            .field("active_count", &self.active.len())
            .finish()
    }
}
