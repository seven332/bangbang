use std::fmt;

use crate::SessionId;

use super::{first_sequence, require_session};
use crate::vmnet_provider::{
    DataMessage, ProviderCleanup, ProviderEnvelope, ProviderFrame, ProviderOperation,
    ProviderStatus, ProviderTerminalCode, RealizedVmnetParameters, VmnetGeneration,
    VmnetInterfaceId, VmnetPacketBatch, VmnetProviderError, VmnetReadinessEpoch, VmnetSequence,
};

/// Worker-side lifecycle for one transferred data stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataClientState {
    /// No hello has been sent.
    New,
    /// The binding acknowledgement is pending.
    AwaitHelloAck,
    /// Packet work or stop may begin.
    Active,
    /// One read or write result is pending.
    AwaitResponse,
    /// Backend stop completion is pending.
    AwaitStopped,
    /// Stop completed; only shutdown remains.
    Stopped,
    /// Orderly shutdown acknowledgement is pending.
    AwaitShutdownAck,
    /// The protocol completed cleanly.
    Closed,
    /// A terminal failure permanently poisoned the state.
    Terminal,
}

/// Owner-side lifecycle for one transferred data stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataOwnerState {
    /// The client binding hello is pending.
    AwaitHello,
    /// The binding acknowledgement must be emitted.
    ReadyToAck,
    /// Packet requests or stop may arrive.
    Active,
    /// One read or write response must be emitted.
    ReadyToRespond,
    /// Backend drain and stop are required.
    ReadyToStop,
    /// Stop completed; only shutdown remains.
    Stopped,
    /// An orderly shutdown acknowledgement is required.
    ReadyToShutdown,
    /// The protocol completed cleanly.
    Closed,
    /// A terminal failure permanently poisoned the state.
    Terminal,
}

/// Checked event produced by the data client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataClientEvent {
    /// The exact stream binding was acknowledged.
    Ready,
    /// One contiguous readiness edge was published.
    Readiness {
        /// Exact readiness epoch.
        epoch: VmnetReadinessEpoch,
        /// Bounded packet estimate.
        estimated_packets: u16,
    },
    /// A read completed with a zero-to-requested batch.
    ReadComplete {
        /// Canonical ordered packet batch.
        packets: VmnetPacketBatch,
    },
    /// A write completed with a zero-to-requested prefix.
    WriteComplete {
        /// Exact successfully written prefix count.
        completed_packets: u16,
    },
    /// A correlated backend operation failed terminally.
    OperationFailed {
        /// Failed operation.
        operation: ProviderOperation,
        /// Stable failure category.
        status: ProviderStatus,
    },
    /// Backend stop completed.
    Stopped,
    /// Orderly stream shutdown completed.
    Shutdown,
    /// The peer declared a terminal category.
    PeerTerminal {
        /// Stable terminal category.
        code: ProviderTerminalCode,
    },
}

/// Checked event produced by the data owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataOwnerEvent {
    /// The exact client binding hello was accepted.
    Hello,
    /// One bounded read requires backend work.
    Read {
        /// Exact request sequence for correlation.
        request: VmnetSequence,
        /// Requested packet maximum.
        max_packets: u16,
    },
    /// One bounded write requires backend work.
    Write {
        /// Exact request sequence for correlation.
        request: VmnetSequence,
        /// Canonical ordered packet batch.
        packets: VmnetPacketBatch,
    },
    /// Callback drain and backend stop are required.
    Stop,
    /// Orderly closure was requested after stop.
    Shutdown,
    /// The peer declared a terminal category.
    PeerTerminal {
        /// Stable terminal category.
        code: ProviderTerminalCode,
    },
}

#[derive(Debug, Clone, Copy)]
enum DataPending {
    Read {
        request: VmnetSequence,
        max_packets: u16,
    },
    Write {
        request: VmnetSequence,
        packet_count: u16,
    },
}

/// Worker-side state machine for one exact interface generation.
pub struct VmnetDataClient {
    session: SessionId,
    interface: VmnetInterfaceId,
    generation: VmnetGeneration,
    parameters: RealizedVmnetParameters,
    state: DataClientState,
    next_local: VmnetSequence,
    next_peer: VmnetSequence,
    next_readiness: VmnetReadinessEpoch,
    readiness_pending: bool,
    pending: Option<DataPending>,
}

impl VmnetDataClient {
    /// Constructs a client bound to the exact transferred-stream scope.
    pub fn new(
        session: SessionId,
        interface: VmnetInterfaceId,
        generation: VmnetGeneration,
        parameters: RealizedVmnetParameters,
    ) -> Result<Self, VmnetProviderError> {
        require_session(session)?;
        Ok(Self {
            session,
            interface,
            generation,
            parameters,
            state: DataClientState::New,
            next_local: first_sequence(),
            next_peer: first_sequence(),
            next_readiness: first_readiness(),
            readiness_pending: false,
            pending: None,
        })
    }

    /// Returns the current lifecycle.
    #[must_use]
    pub const fn state(&self) -> DataClientState {
        self.state
    }

    /// Begins the mandatory exact-binding handshake.
    pub fn hello(&mut self) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(DataClientState::New)?;
        let frame = self.data(DataMessage::Hello)?;
        self.state = DataClientState::AwaitHelloAck;
        Ok(frame)
    }

    /// Begins one bounded read request.
    pub fn read(&mut self, max_packets: u16) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(DataClientState::Active)?;
        if max_packets == 0 || max_packets > self.parameters.effective_read_max_packets() {
            return Err(VmnetProviderError::LimitExceeded);
        }
        let request = self.next_local;
        let frame = self.data(DataMessage::Read { max_packets })?;
        self.pending = Some(DataPending::Read {
            request,
            max_packets,
        });
        self.readiness_pending = false;
        self.state = DataClientState::AwaitResponse;
        Ok(frame)
    }

    /// Begins one bounded nonempty write request.
    pub fn write(
        &mut self,
        packets: VmnetPacketBatch,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(DataClientState::Active)?;
        if !packets.fits(
            self.parameters.packet_buffer_bytes(),
            usize::from(self.parameters.effective_write_max_packets()),
        ) {
            return Err(VmnetProviderError::LimitExceeded);
        }
        let packet_count =
            u16::try_from(packets.packet_count()).map_err(|_| VmnetProviderError::LimitExceeded)?;
        let request = self.next_local;
        let frame = self.data(DataMessage::Write { packets })?;
        self.pending = Some(DataPending::Write {
            request,
            packet_count,
        });
        self.state = DataClientState::AwaitResponse;
        Ok(frame)
    }

    /// Begins callback drain and backend stop.
    pub fn stop(&mut self) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(DataClientState::Active)?;
        let frame = self.data(DataMessage::Stop)?;
        self.readiness_pending = false;
        self.state = DataClientState::AwaitStopped;
        Ok(frame)
    }

    /// Begins orderly closure after complete stop.
    pub fn shutdown(&mut self) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(DataClientState::Stopped)?;
        let frame = self.data(DataMessage::Shutdown)?;
        self.state = DataClientState::AwaitShutdownAck;
        Ok(frame)
    }

    /// Emits a terminal frame and irreversibly clears pending packet ownership.
    pub fn terminal(
        &mut self,
        code: ProviderTerminalCode,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        if matches!(
            self.state,
            DataClientState::Closed | DataClientState::Terminal
        ) {
            return Err(self.local_error());
        }
        let frame = self.data(DataMessage::Terminal { code })?;
        self.poison();
        Ok(frame)
    }

    /// Consumes one checked owner frame.
    pub fn receive(
        &mut self,
        envelope: ProviderEnvelope,
    ) -> Result<DataClientEvent, VmnetProviderError> {
        if matches!(
            self.state,
            DataClientState::Closed | DataClientState::Terminal
        ) {
            return Err(self.local_error());
        }
        let (frame, stream) = envelope.into_parts();
        if stream.is_some() || self.validate_peer(&frame).is_err() {
            return Err(self.peer_error());
        }
        let message = frame.data_message().cloned();
        let result = (|| match (self.state, message) {
            (DataClientState::AwaitHelloAck, Some(DataMessage::HelloAck)) => {
                self.state = DataClientState::Active;
                Ok(DataClientEvent::Ready)
            }
            (
                DataClientState::Active
                | DataClientState::AwaitResponse
                | DataClientState::AwaitStopped,
                Some(DataMessage::Readiness {
                    epoch,
                    estimated_packets,
                }),
            ) => self.accept_readiness(epoch, estimated_packets),
            (
                DataClientState::AwaitResponse,
                Some(DataMessage::ReadResult { request, packets }),
            ) => {
                let DataPending::Read {
                    request: expected,
                    max_packets,
                } = self.pending.ok_or(VmnetProviderError::InvalidPeerState)?
                else {
                    return Err(self.peer_error());
                };
                if request != expected
                    || packets.packet_count() > usize::from(max_packets)
                    || !packets.fits(
                        self.parameters.packet_buffer_bytes(),
                        usize::from(max_packets),
                    )
                {
                    Err(VmnetProviderError::InvalidPeerState)
                } else {
                    self.pending = None;
                    self.state = DataClientState::Active;
                    Ok(DataClientEvent::ReadComplete { packets })
                }
            }
            (
                DataClientState::AwaitResponse,
                Some(DataMessage::WriteResult {
                    request,
                    completed_packets,
                }),
            ) => {
                let DataPending::Write {
                    request: expected,
                    packet_count,
                } = self.pending.ok_or(VmnetProviderError::InvalidPeerState)?
                else {
                    return Err(self.peer_error());
                };
                if request != expected || completed_packets > packet_count {
                    Err(VmnetProviderError::InvalidPeerState)
                } else {
                    self.pending = None;
                    self.state = DataClientState::Active;
                    Ok(DataClientEvent::WriteComplete { completed_packets })
                }
            }
            (
                DataClientState::AwaitResponse,
                Some(DataMessage::OperationFailed {
                    request,
                    operation,
                    status,
                }),
            ) if self.pending_matches(request, operation) => {
                self.poison();
                Ok(DataClientEvent::OperationFailed { operation, status })
            }
            (
                DataClientState::AwaitStopped,
                Some(DataMessage::Stopped {
                    cleanup: ProviderCleanup::Complete,
                }),
            ) => {
                self.state = DataClientState::Stopped;
                Ok(DataClientEvent::Stopped)
            }
            (DataClientState::AwaitShutdownAck, Some(DataMessage::ShutdownAck)) => {
                self.state = DataClientState::Closed;
                Ok(DataClientEvent::Shutdown)
            }
            (_, Some(DataMessage::Terminal { code })) => {
                self.poison();
                Ok(DataClientEvent::PeerTerminal { code })
            }
            _ => Err(VmnetProviderError::InvalidPeerState),
        })();
        if result.is_err() {
            self.poison();
        }
        result
    }
}

impl VmnetDataClient {
    fn accept_readiness(
        &mut self,
        epoch: VmnetReadinessEpoch,
        estimated_packets: u16,
    ) -> Result<DataClientEvent, VmnetProviderError> {
        if self.readiness_pending
            || epoch != self.next_readiness
            || estimated_packets == 0
            || estimated_packets > self.parameters.effective_read_max_packets()
        {
            return Err(VmnetProviderError::InvalidPeerState);
        }
        self.next_readiness = self.next_readiness.checked_next()?;
        self.readiness_pending = true;
        Ok(DataClientEvent::Readiness {
            epoch,
            estimated_packets,
        })
    }

    fn pending_matches(&self, request: VmnetSequence, operation: ProviderOperation) -> bool {
        matches!(
            (self.pending, operation),
            (Some(DataPending::Read { request: expected, .. }), ProviderOperation::Read)
                | (Some(DataPending::Write { request: expected, .. }), ProviderOperation::Write)
                    if request == expected
        )
    }

    fn data(&mut self, message: DataMessage) -> Result<ProviderEnvelope, VmnetProviderError> {
        let sequence = self.take_local_sequence()?;
        ProviderFrame::data(
            self.session,
            self.interface,
            self.generation,
            sequence,
            message,
        )
        .map(ProviderEnvelope::frame_only)
    }

    fn validate_peer(&mut self, frame: &ProviderFrame) -> Result<(), VmnetProviderError> {
        if frame.session() != self.session
            || frame.interface() != Some(self.interface)
            || frame.generation() != Some(self.generation)
            || frame.sequence() != self.next_peer
            || frame.data_message().is_none()
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

    fn require_state(&self, expected: DataClientState) -> Result<(), VmnetProviderError> {
        if self.state == expected {
            Ok(())
        } else {
            Err(self.local_error())
        }
    }

    fn local_error(&self) -> VmnetProviderError {
        if self.state == DataClientState::Terminal {
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
        self.readiness_pending = false;
        self.state = DataClientState::Terminal;
    }
}

impl fmt::Debug for VmnetDataClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetDataClient")
            .field("binding", &"<redacted>")
            .field("state", &self.state)
            .field("pending", &self.pending.as_ref().map(|_| "<owned>"))
            .finish()
    }
}

/// Owner-side state machine for one exact interface generation.
pub struct VmnetDataOwner {
    session: SessionId,
    interface: VmnetInterfaceId,
    generation: VmnetGeneration,
    parameters: RealizedVmnetParameters,
    state: DataOwnerState,
    next_local: VmnetSequence,
    next_peer: VmnetSequence,
    next_readiness: VmnetReadinessEpoch,
    readiness_outstanding: bool,
    pending: Option<DataPending>,
}

impl VmnetDataOwner {
    /// Constructs an owner bound to the exact transferred-stream scope.
    pub fn new(
        session: SessionId,
        interface: VmnetInterfaceId,
        generation: VmnetGeneration,
        parameters: RealizedVmnetParameters,
    ) -> Result<Self, VmnetProviderError> {
        require_session(session)?;
        Ok(Self {
            session,
            interface,
            generation,
            parameters,
            state: DataOwnerState::AwaitHello,
            next_local: first_sequence(),
            next_peer: first_sequence(),
            next_readiness: first_readiness(),
            readiness_outstanding: false,
            pending: None,
        })
    }

    /// Returns the current lifecycle.
    #[must_use]
    pub const fn state(&self) -> DataOwnerState {
        self.state
    }

    /// Consumes one checked client frame.
    pub fn receive(
        &mut self,
        envelope: ProviderEnvelope,
    ) -> Result<DataOwnerEvent, VmnetProviderError> {
        if matches!(
            self.state,
            DataOwnerState::Closed | DataOwnerState::Terminal
        ) {
            return Err(self.local_error());
        }
        let (frame, stream) = envelope.into_parts();
        if stream.is_some() || self.validate_peer(&frame).is_err() {
            return Err(self.peer_error());
        }
        let message = frame.data_message().cloned();
        let result = (|| match (self.state, message) {
            (DataOwnerState::AwaitHello, Some(DataMessage::Hello)) => {
                self.state = DataOwnerState::ReadyToAck;
                Ok(DataOwnerEvent::Hello)
            }
            (DataOwnerState::Active, Some(DataMessage::Read { max_packets })) => {
                if max_packets == 0 || max_packets > self.parameters.effective_read_max_packets() {
                    Err(VmnetProviderError::InvalidPeerState)
                } else {
                    let request = frame.sequence();
                    self.pending = Some(DataPending::Read {
                        request,
                        max_packets,
                    });
                    self.readiness_outstanding = false;
                    self.state = DataOwnerState::ReadyToRespond;
                    Ok(DataOwnerEvent::Read {
                        request,
                        max_packets,
                    })
                }
            }
            (DataOwnerState::Active, Some(DataMessage::Write { packets })) => {
                if !packets.fits(
                    self.parameters.packet_buffer_bytes(),
                    usize::from(self.parameters.effective_write_max_packets()),
                ) {
                    Err(VmnetProviderError::InvalidPeerState)
                } else {
                    let request = frame.sequence();
                    let packet_count = u16::try_from(packets.packet_count())
                        .map_err(|_| VmnetProviderError::InvalidPeerState)?;
                    self.pending = Some(DataPending::Write {
                        request,
                        packet_count,
                    });
                    self.state = DataOwnerState::ReadyToRespond;
                    Ok(DataOwnerEvent::Write { request, packets })
                }
            }
            (DataOwnerState::Active, Some(DataMessage::Stop)) => {
                self.readiness_outstanding = false;
                self.state = DataOwnerState::ReadyToStop;
                Ok(DataOwnerEvent::Stop)
            }
            (DataOwnerState::Stopped, Some(DataMessage::Shutdown)) => {
                self.state = DataOwnerState::ReadyToShutdown;
                Ok(DataOwnerEvent::Shutdown)
            }
            (_, Some(DataMessage::Terminal { code })) => {
                self.poison();
                Ok(DataOwnerEvent::PeerTerminal { code })
            }
            _ => Err(VmnetProviderError::InvalidPeerState),
        })();
        if result.is_err() {
            self.poison();
        }
        result
    }

    /// Emits the mandatory exact-binding acknowledgement.
    pub fn hello_ack(&mut self) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(DataOwnerState::ReadyToAck)?;
        let frame = self.data(DataMessage::HelloAck)?;
        self.state = DataOwnerState::Active;
        Ok(frame)
    }

    /// Publishes one coalesced readiness edge.
    pub fn readiness(
        &mut self,
        estimated_packets: u16,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        if !matches!(
            self.state,
            DataOwnerState::Active | DataOwnerState::ReadyToRespond
        ) {
            return Err(self.local_error());
        }
        if self.readiness_outstanding
            || estimated_packets == 0
            || estimated_packets > self.parameters.effective_read_max_packets()
        {
            return Err(VmnetProviderError::LimitExceeded);
        }
        let epoch = self.next_readiness;
        let frame = self.data(DataMessage::Readiness {
            epoch,
            estimated_packets,
        })?;
        self.next_readiness = self.next_readiness.checked_next()?;
        self.readiness_outstanding = true;
        Ok(frame)
    }

    /// Completes one pending read with a zero-to-requested batch.
    pub fn read_result(
        &mut self,
        packets: VmnetPacketBatch,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(DataOwnerState::ReadyToRespond)?;
        let DataPending::Read {
            request,
            max_packets,
        } = self.pending.ok_or(VmnetProviderError::InvalidLifecycle)?
        else {
            return Err(VmnetProviderError::InvalidLifecycle);
        };
        if packets.packet_count() > usize::from(max_packets)
            || !packets.fits(
                self.parameters.packet_buffer_bytes(),
                usize::from(max_packets),
            )
        {
            return Err(VmnetProviderError::LimitExceeded);
        }
        let frame = self.data(DataMessage::ReadResult { request, packets })?;
        self.pending = None;
        self.state = DataOwnerState::Active;
        Ok(frame)
    }

    /// Completes one pending write with a zero-to-requested prefix.
    pub fn write_result(
        &mut self,
        completed_packets: u16,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(DataOwnerState::ReadyToRespond)?;
        let DataPending::Write {
            request,
            packet_count,
        } = self.pending.ok_or(VmnetProviderError::InvalidLifecycle)?
        else {
            return Err(VmnetProviderError::InvalidLifecycle);
        };
        if completed_packets > packet_count {
            return Err(VmnetProviderError::LimitExceeded);
        }
        let frame = self.data(DataMessage::WriteResult {
            request,
            completed_packets,
        })?;
        self.pending = None;
        self.state = DataOwnerState::Active;
        Ok(frame)
    }

    /// Reports a correlated terminal backend failure.
    pub fn operation_failed(
        &mut self,
        status: ProviderStatus,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(DataOwnerState::ReadyToRespond)?;
        let pending = self.pending.ok_or(VmnetProviderError::InvalidLifecycle)?;
        let (request, operation) = match pending {
            DataPending::Read { request, .. } => (request, ProviderOperation::Read),
            DataPending::Write { request, .. } => (request, ProviderOperation::Write),
        };
        let frame = self.data(DataMessage::OperationFailed {
            request,
            operation,
            status,
        })?;
        self.poison();
        Ok(frame)
    }

    /// Completes callback drain and backend stop.
    pub fn stopped(
        &mut self,
        cleanup: ProviderCleanup,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(DataOwnerState::ReadyToStop)?;
        let frame = self.data(DataMessage::Stopped { cleanup })?;
        self.state = if cleanup == ProviderCleanup::Complete {
            DataOwnerState::Stopped
        } else {
            DataOwnerState::Terminal
        };
        Ok(frame)
    }

    /// Completes orderly stream closure.
    pub fn shutdown_ack(&mut self) -> Result<ProviderEnvelope, VmnetProviderError> {
        self.require_state(DataOwnerState::ReadyToShutdown)?;
        let frame = self.data(DataMessage::ShutdownAck)?;
        self.state = DataOwnerState::Closed;
        Ok(frame)
    }

    /// Emits a terminal frame and irreversibly clears pending packet ownership.
    pub fn terminal(
        &mut self,
        code: ProviderTerminalCode,
    ) -> Result<ProviderEnvelope, VmnetProviderError> {
        if matches!(
            self.state,
            DataOwnerState::Closed | DataOwnerState::Terminal
        ) {
            return Err(self.local_error());
        }
        let frame = self.data(DataMessage::Terminal { code })?;
        self.poison();
        Ok(frame)
    }
}

impl VmnetDataOwner {
    fn data(&mut self, message: DataMessage) -> Result<ProviderEnvelope, VmnetProviderError> {
        let sequence = self.take_local_sequence()?;
        ProviderFrame::data(
            self.session,
            self.interface,
            self.generation,
            sequence,
            message,
        )
        .map(ProviderEnvelope::frame_only)
    }

    fn validate_peer(&mut self, frame: &ProviderFrame) -> Result<(), VmnetProviderError> {
        if frame.session() != self.session
            || frame.interface() != Some(self.interface)
            || frame.generation() != Some(self.generation)
            || frame.sequence() != self.next_peer
            || frame.data_message().is_none()
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

    fn require_state(&self, expected: DataOwnerState) -> Result<(), VmnetProviderError> {
        if self.state == expected {
            Ok(())
        } else {
            Err(self.local_error())
        }
    }

    fn local_error(&self) -> VmnetProviderError {
        if self.state == DataOwnerState::Terminal {
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
        self.readiness_outstanding = false;
        self.state = DataOwnerState::Terminal;
    }
}

impl fmt::Debug for VmnetDataOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetDataOwner")
            .field("binding", &"<redacted>")
            .field("state", &self.state)
            .field("pending", &self.pending.as_ref().map(|_| "<owned>"))
            .finish()
    }
}

fn first_readiness() -> VmnetReadinessEpoch {
    VmnetReadinessEpoch::MIN
}
