use crate::{Frame, Message, ProtocolError, Readiness, Role, SessionId};

/// Launcher-observed monotonic lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LauncherState {
    /// Suspended worker was resumed; fixed pre-session greeting is pending.
    AwaitHello,
    /// Valid `Hello` arrived; the random-session `Start` may be sent.
    ReadyToStart,
    /// `Start` was sent; namespace proof is pending.
    AwaitPrepared,
    /// A valid namespace proof arrived and local validation may authorize it.
    ReadyToProceed,
    /// Namespace was accepted and `Proceed` was sent.
    AwaitStarting,
    /// Worker entered public command/startup processing.
    Starting,
    /// Worker reported committed API/no-API readiness.
    Ready(Readiness),
    /// Worker reported its structured terminal result.
    Terminal,
}

/// Worker-observed monotonic lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerState {
    /// No valid `Start` has been received.
    AwaitStart,
    /// `Start` was accepted; the namespace has not yet been reported.
    Preparing,
    /// Namespace is locked and `Proceed` or early cancellation is pending.
    AwaitProceed,
    /// Public command/startup processing has begun.
    Starting,
    /// API/no-API readiness is committed.
    Ready(Readiness),
    /// Structured terminal status was sent.
    Terminal,
}

/// Launcher-side role, session, sequence, and lifecycle enforcement.
#[derive(Debug)]
pub struct LauncherLifecycle {
    session: SessionId,
    state: LauncherState,
    incoming_sequence: u64,
    outgoing_sequence: u64,
    cancelled: bool,
}

impl LauncherLifecycle {
    /// Creates state after the launcher has selected a fresh identity.
    #[must_use]
    pub const fn new(session: SessionId) -> Self {
        Self {
            session,
            state: LauncherState::AwaitHello,
            incoming_sequence: 0,
            outgoing_sequence: 0,
            cancelled: false,
        }
    }

    /// Returns the bound private session identity.
    #[must_use]
    pub const fn session(&self) -> SessionId {
        self.session
    }

    /// Returns current monotonic state.
    #[must_use]
    pub const fn state(&self) -> LauncherState {
        self.state
    }

    /// Returns whether the launcher has already sent cancellation.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Creates the initial `Start` frame.
    pub fn start(&mut self) -> Result<Frame, ProtocolError> {
        if self.session.is_pre_session()
            || self.outgoing_sequence != 0
            || self.state != LauncherState::ReadyToStart
        {
            return Err(ProtocolError::InvalidLifecycle);
        }
        self.state = LauncherState::AwaitPrepared;
        self.outgoing(Message::Start)
    }

    /// Creates `Proceed` after local namespace validation.
    pub fn proceed(&mut self) -> Result<Frame, ProtocolError> {
        if self.state != LauncherState::ReadyToProceed || self.cancelled {
            return Err(ProtocolError::InvalidLifecycle);
        }
        self.state = LauncherState::AwaitStarting;
        self.outgoing(Message::Proceed)
    }

    /// Creates the only cancellation frame for this session.
    pub fn cancel(&mut self, signal: crate::CancelSignal) -> Result<Frame, ProtocolError> {
        if self.cancelled
            || !matches!(
                self.state,
                LauncherState::AwaitPrepared
                    | LauncherState::ReadyToProceed
                    | LauncherState::AwaitStarting
                    | LauncherState::Starting
                    | LauncherState::Ready(_)
            )
        {
            return Err(ProtocolError::InvalidLifecycle);
        }
        self.cancelled = true;
        self.outgoing(Message::Cancel(signal))
    }

    /// Validates and applies one worker frame.
    pub fn receive(&mut self, frame: Frame) -> Result<Message, ProtocolError> {
        if self.state == LauncherState::AwaitHello {
            if frame.session != SessionId::pre_session()
                || frame.sequence != 0
                || frame.message != Message::Hello
            {
                return Err(ProtocolError::InvalidPeerState);
            }
            self.state = LauncherState::ReadyToStart;
            self.incoming_sequence = 1;
            return Ok(Message::Hello);
        }
        self.validate_incoming(frame, Role::Worker)?;
        match (self.state, frame.message) {
            (LauncherState::AwaitPrepared, Message::Prepared { .. }) => {
                self.state = LauncherState::ReadyToProceed;
            }
            (LauncherState::AwaitStarting, Message::Starting) => {
                self.state = LauncherState::Starting;
            }
            (LauncherState::Starting, Message::Ready(readiness)) => {
                self.state = LauncherState::Ready(readiness);
            }
            (LauncherState::Starting | LauncherState::Ready(_), Message::Terminal { .. }) => {
                self.state = LauncherState::Terminal;
            }
            // A cancelled bootstrap may still report Prepared before it sees Cancel.
            (
                LauncherState::AwaitPrepared | LauncherState::ReadyToProceed,
                Message::Terminal { .. },
            ) if self.cancelled => {
                self.state = LauncherState::Terminal;
            }
            _ => return Err(ProtocolError::InvalidLifecycle),
        }
        self.incoming_sequence = self
            .incoming_sequence
            .checked_add(1)
            .ok_or(ProtocolError::InvalidPeerState)?;
        Ok(frame.message)
    }

    fn outgoing(&mut self, message: Message) -> Result<Frame, ProtocolError> {
        if message.sender() != Role::Launcher {
            return Err(ProtocolError::InvalidPeerState);
        }
        let frame = Frame {
            session: self.session,
            sequence: self.outgoing_sequence,
            message,
        };
        self.outgoing_sequence = self
            .outgoing_sequence
            .checked_add(1)
            .ok_or(ProtocolError::InvalidPeerState)?;
        Ok(frame)
    }

    fn validate_incoming(&self, frame: Frame, role: Role) -> Result<(), ProtocolError> {
        if frame.session != self.session
            || frame.sequence != self.incoming_sequence
            || frame.message.sender() != role
        {
            return Err(ProtocolError::InvalidPeerState);
        }
        Ok(())
    }
}

/// Worker-side role, session, sequence, and lifecycle enforcement.
#[derive(Debug)]
pub struct WorkerLifecycle {
    session: Option<SessionId>,
    state: WorkerState,
    incoming_sequence: u64,
    outgoing_sequence: u64,
    cancelled: bool,
}

impl WorkerLifecycle {
    /// Creates an unbound worker bootstrap state.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            session: None,
            state: WorkerState::AwaitStart,
            incoming_sequence: 0,
            outgoing_sequence: 0,
            cancelled: false,
        }
    }

    /// Returns the identity after a valid `Start` frame.
    #[must_use]
    pub const fn session(&self) -> Option<SessionId> {
        self.session
    }

    /// Returns current monotonic state.
    #[must_use]
    pub const fn state(&self) -> WorkerState {
        self.state
    }

    /// Returns whether a valid cancellation was received.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Validates and applies one launcher frame.
    pub fn receive(&mut self, frame: Frame) -> Result<Message, ProtocolError> {
        if frame.message.sender() != Role::Launcher || frame.sequence != self.incoming_sequence {
            return Err(ProtocolError::InvalidPeerState);
        }
        if let Some(session) = self.session
            && frame.session != session
        {
            return Err(ProtocolError::InvalidPeerState);
        }

        match (self.state, frame.message) {
            (WorkerState::AwaitStart, Message::Start) if !frame.session.is_pre_session() => {
                self.session = Some(frame.session);
                self.state = WorkerState::Preparing;
            }
            (WorkerState::AwaitProceed, Message::Proceed) if !self.cancelled => {
                self.state = WorkerState::Starting;
            }
            (
                WorkerState::Preparing
                | WorkerState::AwaitProceed
                | WorkerState::Starting
                | WorkerState::Ready(_),
                Message::Cancel(_),
            ) if !self.cancelled => {
                self.cancelled = true;
            }
            _ => return Err(ProtocolError::InvalidLifecycle),
        }
        self.incoming_sequence = self
            .incoming_sequence
            .checked_add(1)
            .ok_or(ProtocolError::InvalidPeerState)?;
        Ok(frame.message)
    }

    /// Creates the only valid reserved pre-session greeting.
    pub fn hello(&mut self) -> Result<Frame, ProtocolError> {
        if self.state != WorkerState::AwaitStart || self.outgoing_sequence != 0 {
            return Err(ProtocolError::InvalidLifecycle);
        }
        let frame = Frame {
            session: SessionId::pre_session(),
            sequence: 0,
            message: Message::Hello,
        };
        self.outgoing_sequence = 1;
        Ok(frame)
    }

    /// Creates the namespace proof after valid `Start`.
    pub fn prepared(&mut self, device: u64, inode: u64) -> Result<Frame, ProtocolError> {
        if self.state != WorkerState::Preparing || self.cancelled {
            return Err(ProtocolError::InvalidLifecycle);
        }
        self.state = WorkerState::AwaitProceed;
        self.outgoing(Message::Prepared { device, inode })
    }

    /// Creates the starting notification after valid `Proceed`.
    pub fn starting(&mut self) -> Result<Frame, ProtocolError> {
        if self.state != WorkerState::Starting || self.cancelled {
            return Err(ProtocolError::InvalidLifecycle);
        }
        self.outgoing(Message::Starting)
    }

    /// Creates committed readiness.
    pub fn ready(&mut self, readiness: Readiness) -> Result<Frame, ProtocolError> {
        if self.state != WorkerState::Starting || self.cancelled {
            return Err(ProtocolError::InvalidLifecycle);
        }
        self.state = WorkerState::Ready(readiness);
        self.outgoing(Message::Ready(readiness))
    }

    /// Creates final structured status from a started or ready worker.
    pub fn terminal(
        &mut self,
        category: crate::TerminalCategory,
        exit_code: u8,
    ) -> Result<Frame, ProtocolError> {
        if !matches!(self.state, WorkerState::Starting | WorkerState::Ready(_)) {
            return Err(ProtocolError::InvalidLifecycle);
        }
        self.state = WorkerState::Terminal;
        self.outgoing(Message::Terminal {
            category,
            exit_code,
        })
    }

    fn outgoing(&mut self, message: Message) -> Result<Frame, ProtocolError> {
        if message.sender() != Role::Worker {
            return Err(ProtocolError::InvalidPeerState);
        }
        let session = self.session.ok_or(ProtocolError::InvalidLifecycle)?;
        let frame = Frame {
            session,
            sequence: self.outgoing_sequence,
            message,
        };
        self.outgoing_sequence = self
            .outgoing_sequence
            .checked_add(1)
            .ok_or(ProtocolError::InvalidPeerState)?;
        Ok(frame)
    }
}

impl Default for WorkerLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::{CancelSignal, TerminalCategory};

    use super::*;

    fn session(byte: u8) -> SessionId {
        SessionId::from_bytes([byte; 32])
    }

    fn exchange_start(launcher: &mut LauncherLifecycle, worker: &mut WorkerLifecycle) {
        let hello = worker.hello().expect("hello should be valid");
        assert_eq!(launcher.receive(hello), Ok(Message::Hello));
        let start = launcher.start().expect("start should be valid");
        assert_eq!(worker.receive(start), Ok(Message::Start));
    }

    #[test]
    fn api_lifecycle_is_monotonic() {
        let mut launcher = LauncherLifecycle::new(session(1));
        let mut worker = WorkerLifecycle::new();
        exchange_start(&mut launcher, &mut worker);
        let prepared = worker.prepared(2, 3).expect("prepared should send");
        assert_eq!(launcher.receive(prepared), Ok(prepared.message));
        let proceed = launcher.proceed().expect("proceed should send");
        assert_eq!(worker.receive(proceed), Ok(Message::Proceed));
        let starting = worker.starting().expect("starting should send");
        assert_eq!(launcher.receive(starting), Ok(Message::Starting));
        let ready = worker.ready(Readiness::Api).expect("ready should send");
        assert_eq!(launcher.receive(ready), Ok(Message::Ready(Readiness::Api)));
        let terminal = worker
            .terminal(TerminalCategory::Success, 0)
            .expect("terminal should send");
        assert_eq!(launcher.receive(terminal), Ok(terminal.message));
        assert_eq!(launcher.state(), LauncherState::Terminal);
    }

    #[test]
    fn early_command_terminates_without_ready() {
        let mut launcher = LauncherLifecycle::new(session(2));
        let mut worker = WorkerLifecycle::new();
        exchange_start(&mut launcher, &mut worker);
        launcher
            .receive(worker.prepared(4, 5).expect("prepared should send"))
            .expect("prepared should receive");
        worker
            .receive(launcher.proceed().expect("proceed should send"))
            .expect("proceed should receive");
        launcher
            .receive(worker.starting().expect("starting should send"))
            .expect("starting should receive");
        let terminal = worker
            .terminal(TerminalCategory::Success, 0)
            .expect("terminal should send");
        launcher.receive(terminal).expect("terminal should receive");
        assert_eq!(launcher.state(), LauncherState::Terminal);
    }

    #[test]
    fn rejects_replay_cross_session_wrong_role_and_state() {
        let mut launcher = LauncherLifecycle::new(session(3));
        let mut worker = WorkerLifecycle::new();
        launcher
            .receive(worker.hello().expect("hello should send"))
            .expect("hello should receive");
        let start = launcher.start().expect("start should send");
        worker.receive(start).expect("start should receive");
        assert_eq!(worker.receive(start), Err(ProtocolError::InvalidPeerState));

        let sequence_gap = Frame {
            session: session(3),
            sequence: 2,
            message: Message::Proceed,
        };
        assert_eq!(
            worker.receive(sequence_gap),
            Err(ProtocolError::InvalidPeerState)
        );

        let mut wrong_session = worker.prepared(8, 9).expect("prepared should send");
        wrong_session.session = session(4);
        assert_eq!(
            launcher.receive(wrong_session),
            Err(ProtocolError::InvalidPeerState)
        );

        let wrong_role = Frame {
            session: session(3),
            sequence: 0,
            message: Message::Proceed,
        };
        assert_eq!(
            launcher.receive(wrong_role),
            Err(ProtocolError::InvalidPeerState)
        );

        assert_eq!(
            worker.ready(Readiness::NoApi),
            Err(ProtocolError::InvalidLifecycle)
        );
    }

    #[test]
    fn cancellation_is_single_and_valid_before_or_after_proceed() {
        let mut launcher = LauncherLifecycle::new(session(5));
        let mut worker = WorkerLifecycle::new();
        exchange_start(&mut launcher, &mut worker);
        let cancel = launcher
            .cancel(CancelSignal::Terminate)
            .expect("first cancel should send");
        assert_eq!(
            worker.receive(cancel),
            Ok(Message::Cancel(CancelSignal::Terminate))
        );
        assert_eq!(
            launcher.cancel(CancelSignal::Interrupt),
            Err(ProtocolError::InvalidLifecycle)
        );
        assert_eq!(worker.prepared(1, 2), Err(ProtocolError::InvalidLifecycle));
    }

    #[test]
    fn reserved_identity_is_accepted_only_for_initial_hello() {
        let mut launcher = LauncherLifecycle::new(SessionId::pre_session());
        let mut worker = WorkerLifecycle::new();
        launcher
            .receive(worker.hello().expect("hello should send"))
            .expect("hello should receive");
        assert_eq!(launcher.start(), Err(ProtocolError::InvalidLifecycle));

        let mut worker = WorkerLifecycle::new();
        assert_eq!(
            worker.receive(Frame {
                session: SessionId::pre_session(),
                sequence: 0,
                message: Message::Start,
            }),
            Err(ProtocolError::InvalidLifecycle)
        );
    }

    #[test]
    fn cancellation_is_rejected_before_a_session_starts() {
        let mut launcher = LauncherLifecycle::new(session(6));
        assert_eq!(
            launcher.cancel(CancelSignal::Interrupt),
            Err(ProtocolError::InvalidLifecycle)
        );
        let mut worker = WorkerLifecycle::new();
        launcher
            .receive(worker.hello().expect("hello should send"))
            .expect("hello should receive");
        assert_eq!(
            launcher.cancel(CancelSignal::Interrupt),
            Err(ProtocolError::InvalidLifecycle)
        );
    }

    #[test]
    fn proceed_requires_a_prepared_frame_on_both_sides() {
        let mut launcher = LauncherLifecycle::new(session(7));
        let mut worker = WorkerLifecycle::new();
        exchange_start(&mut launcher, &mut worker);
        assert_eq!(launcher.proceed(), Err(ProtocolError::InvalidLifecycle));
        assert_eq!(
            worker.receive(Frame {
                session: session(7),
                sequence: 1,
                message: Message::Proceed,
            }),
            Err(ProtocolError::InvalidLifecycle)
        );

        let prepared = worker.prepared(10, 11).expect("prepared should send");
        launcher.receive(prepared).expect("prepared should receive");
        let proceed = launcher.proceed().expect("proceed should now send");
        assert_eq!(worker.receive(proceed), Ok(Message::Proceed));
    }
}
